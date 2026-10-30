use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};

use crate::{
    application::user_access_rules::{
        MAX_ACTIVE_IP_BLACKLIST_RULES_PER_REQUEST, UserAccessBlacklistRuleType,
        ip_blacklist_rules_match_address, normalize_user_access_blacklist_value,
    },
    auth::{
        password::PasswordError,
        users::{UserRecord, UserStore, UserStoreError},
    },
    domain::ids::UserId,
    storage::{
        AuthCacheEpochSnapshot, Database, NewAccessToken, NewPlaybackAccessToken, StorageError,
        StoredEmbyLoginSessionSummary, UserAccessBlacklistIpRuleQuery,
        UserAccessBlacklistRuleMatchQuery,
    },
};

use super::cache::ExpiringCache;

/// Activity columns (`access_tokens.last_seen_at`, `user_devices.last_seen_at`)
/// do not need second-level precision. Refreshing them on every Emby request
/// turns browsing and image loads into one write transaction per request, so
/// keep a short in-process throttle and let the database see at most one
/// refresh per window.
const ACCESS_TOKEN_TOUCH_INTERVAL: Duration = Duration::from_secs(60);
const DEVICE_ACCESS_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);
const ACTIVITY_CACHE_MAX_ENTRIES: usize = 4_096;
const ACTIVITY_CACHE_RETENTION: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Default)]
struct EmbyActivityCache {
    token_touches: HashMap<Vec<u8>, Instant>,
    device_refreshes: HashMap<Vec<u8>, Instant>,
}

impl EmbyActivityCache {
    fn should_touch(&self, token_hash: &[u8], now: Instant) -> bool {
        self.token_touches
            .get(token_hash)
            .is_none_or(|last| now.duration_since(*last) >= ACCESS_TOKEN_TOUCH_INTERVAL)
    }

    fn mark_touched(&mut self, token_hash: &[u8], now: Instant) {
        self.token_touches.insert(token_hash.to_vec(), now);
        self.prune(now);
    }

    fn device_refresh_due(&self, token_hash: &[u8], now: Instant) -> bool {
        self.device_refreshes
            .get(token_hash)
            .is_none_or(|last| now.duration_since(*last) >= DEVICE_ACCESS_REFRESH_INTERVAL)
    }

    fn mark_device_refreshed(&mut self, token_hash: &[u8], now: Instant) {
        self.device_refreshes.insert(token_hash.to_vec(), now);
        self.prune(now);
    }

    fn prune(&mut self, now: Instant) {
        if self.token_touches.len() + self.device_refreshes.len() <= ACTIVITY_CACHE_MAX_ENTRIES {
            return;
        }
        self.token_touches
            .retain(|_, last| now.duration_since(*last) < ACTIVITY_CACHE_RETENTION);
        self.device_refreshes
            .retain(|_, last| now.duration_since(*last) < ACTIVITY_CACHE_RETENTION);
        if self.token_touches.len() + self.device_refreshes.len() > ACTIVITY_CACHE_MAX_ENTRIES {
            self.token_touches.clear();
            self.device_refreshes.clear();
        }
    }
}

/// A resolved Emby credential: the token's user and the device identity it was issued to.
///
/// The access rules are deliberately *not* cached: they are re-read on every request (one query)
/// and evaluated against the address the access-IP resolver produced. The rule tables are the only
/// ACL input that can change without any product code path, so keeping them out of the cache means
/// a rule change can never be served stale.
struct CachedEmbyPrincipal {
    user: UserRecord,
    device: EmbyDeviceInfo,
    /// Captured before the credential was read; a hit is only served while it is still current.
    snapshot: AuthCacheEpochSnapshot,
}

#[derive(Clone)]
pub struct EmbyAuthService {
    database: Database,
    users: UserStore,
    activity_cache: Arc<Mutex<EmbyActivityCache>>,
    principal_cache: Arc<ExpiringCache<CachedEmbyPrincipal>>,
}

impl EmbyAuthService {
    pub fn new(database: Database) -> Result<Self, PasswordError> {
        Ok(Self {
            users: UserStore::new(database.clone())?,
            database,
            activity_cache: Arc::new(Mutex::new(EmbyActivityCache::default())),
            principal_cache: Arc::new(ExpiringCache::default()),
        })
    }

    pub async fn public_users(&self) -> Result<Vec<UserRecord>, EmbyAuthError> {
        let stored_users = self.database.list_users().await?;
        stored_users.into_iter().map(user_record).collect()
    }

    pub async fn query_users(
        &self,
        is_disabled: Option<bool>,
        name_starts_with_or_greater: Option<&str>,
        descending: bool,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<UserRecord>, i64), EmbyAuthError> {
        let (stored_users, total_count) = self
            .database
            .query_users(
                is_disabled,
                name_starts_with_or_greater,
                descending,
                offset,
                limit,
            )
            .await?;
        let users = stored_users
            .into_iter()
            .map(user_record)
            .collect::<Result<Vec<_>, _>>()?;
        Ok((users, total_count))
    }

    pub async fn user_by_id(&self, user_id: &str) -> Result<Option<UserRecord>, EmbyAuthError> {
        Ok(self.users.find_by_id(user_id).await?)
    }

    pub async fn authenticate(
        &self,
        username: &str,
        password: &str,
        device: &EmbyDeviceInfo,
    ) -> Result<Option<EmbyAuthResult>, EmbyAuthError> {
        self.authenticate_with_ip(username, password, device, None)
            .await
    }

    pub async fn authenticate_with_ip(
        &self,
        username: &str,
        password: &str,
        device: &EmbyDeviceInfo,
        client_ip: Option<IpAddr>,
    ) -> Result<Option<EmbyAuthResult>, EmbyAuthError> {
        let Some(user) = self
            .users
            .authenticate_emby_name(username, password)
            .await?
        else {
            return Ok(None);
        };
        self.create_access_token(user, device, client_ip).await
    }

    pub async fn find_failed_authentication_user_id(
        &self,
        username: &str,
    ) -> Result<Option<String>, EmbyAuthError> {
        Ok(self
            .users
            .find_emby_authentication_target_id(username)
            .await?)
    }

    pub async fn authenticate_user_id(
        &self,
        user_id: &str,
        password: &str,
        device: &EmbyDeviceInfo,
    ) -> Result<Option<EmbyAuthResult>, EmbyAuthError> {
        self.authenticate_user_id_with_ip(user_id, password, device, None)
            .await
    }

    pub async fn authenticate_user_id_with_ip(
        &self,
        user_id: &str,
        password: &str,
        device: &EmbyDeviceInfo,
        client_ip: Option<IpAddr>,
    ) -> Result<Option<EmbyAuthResult>, EmbyAuthError> {
        let Some(user) = self.users.authenticate_by_id(user_id, password).await? else {
            return Ok(None);
        };
        self.create_access_token(user, device, client_ip).await
    }

    async fn create_access_token(
        &self,
        user: UserRecord,
        device: &EmbyDeviceInfo,
        client_ip: Option<IpAddr>,
    ) -> Result<Option<EmbyAuthResult>, EmbyAuthError> {
        if self
            .user_access_blacklist_matches(&user.id.to_string(), device)
            .await?
        {
            return Ok(None);
        }
        if self
            .is_user_ip_blacklisted(&user.id.to_string(), client_ip)
            .await?
        {
            return Ok(None);
        }
        let token = generate_token()?;
        let token_id = UserId::new().to_string();
        let token_hash = hash_token(&token);
        let user_id = user.id.to_string();
        self.database
            .create_access_token(NewAccessToken {
                id: &token_id,
                token_hash: &token_hash,
                user_id: &user_id,
                device_id: &device.device_id,
                client_name: &device.client,
                device_name: &device.device,
                client_version: &device.version,
                device_type: None,
            })
            .await?;
        Ok(Some(EmbyAuthResult {
            token,
            session_id: token_id,
            user: user.clone(),
            device: device.clone(),
        }))
    }

    async fn user_access_blacklist_matches(
        &self,
        user_id: &str,
        device: &EmbyDeviceInfo,
    ) -> Result<bool, EmbyAuthError> {
        let device_id = normalize_user_access_blacklist_value(
            UserAccessBlacklistRuleType::Device,
            &device.device_id,
        )
        .ok();
        let client_name = normalize_user_access_blacklist_value(
            UserAccessBlacklistRuleType::Client,
            &device.client,
        )
        .ok();
        Ok(self
            .database
            .user_access_blacklist_matches(UserAccessBlacklistRuleMatchQuery {
                user_id,
                device_id: device_id.as_deref(),
                client_name: client_name.as_deref(),
            })
            .await?)
    }

    async fn is_user_ip_blacklisted(
        &self,
        user_id: &str,
        client_ip: Option<IpAddr>,
    ) -> Result<bool, EmbyAuthError> {
        let rules = self
            .database
            .list_user_access_ip_blacklist_rule_values(UserAccessBlacklistIpRuleQuery {
                user_id,
                limit: MAX_ACTIVE_IP_BLACKLIST_RULES_PER_REQUEST as i64 + 1,
            })
            .await
            .map_err(|_| EmbyAuthError::AccessRulesUnavailable)?;
        if rules.len() > MAX_ACTIVE_IP_BLACKLIST_RULES_PER_REQUEST {
            return Err(EmbyAuthError::AccessRulesUnavailable);
        }
        if rules.is_empty() {
            return Ok(false);
        }
        let Some(client_ip) = client_ip else {
            return Ok(true);
        };
        ip_blacklist_rules_match_address(client_ip, &rules)
            .map_err(|_| EmbyAuthError::AccessRulesUnavailable)
    }

    pub async fn logout(&self, token: &str) -> Result<(), EmbyAuthError> {
        self.database
            .revoke_access_token(&hash_token(token))
            .await?;
        Ok(())
    }

    pub async fn list_admin_login_sessions(
        &self,
        user_id: &str,
        page_size: i64,
        offset: i64,
    ) -> Result<(Vec<EmbyLoginSessionSummary>, i64), EmbyAuthError> {
        let (sessions, total_count) = self
            .database
            .list_user_emby_login_sessions_for_admin(user_id, page_size, offset)
            .await?;
        Ok((
            sessions
                .into_iter()
                .map(EmbyLoginSessionSummary::from)
                .collect(),
            total_count,
        ))
    }

    pub async fn revoke_admin_login_session(
        &self,
        user_id: &str,
        session_id: &str,
    ) -> Result<bool, EmbyAuthError> {
        Ok(self
            .database
            .revoke_user_emby_login_session_for_admin(user_id, session_id)
            .await?)
    }

    pub async fn revoke_all_admin_login_sessions(
        &self,
        user_id: &str,
    ) -> Result<u64, EmbyAuthError> {
        Ok(self
            .database
            .revoke_all_user_emby_login_sessions_for_admin(user_id)
            .await?)
    }

    pub async fn verify_token(&self, token: &str) -> Result<bool, EmbyAuthError> {
        Ok(self
            .database
            .has_valid_access_token(&hash_token(token))
            .await?)
    }

    pub async fn device_info(&self, token: &str) -> Result<Option<EmbyDeviceInfo>, EmbyAuthError> {
        Ok(self
            .database
            .find_access_token_device(&hash_token(token))
            .await?
            .map(|device| EmbyDeviceInfo {
                client: device.client_name,
                device: device.device_name,
                device_id: device.device_id,
                version: device.client_version,
                user_id: None,
            }))
    }

    pub async fn resolve_token(&self, token: &str) -> Result<Option<UserRecord>, EmbyAuthError> {
        self.resolve_token_with_ip(token, None).await
    }

    pub async fn resolve_token_with_ip(
        &self,
        token: &str,
        client_ip: Option<IpAddr>,
    ) -> Result<Option<UserRecord>, EmbyAuthError> {
        self.resolve_token_scoped(token, client_ip, false).await
    }

    /// Resolver for the Emby-compatible routes only: unlike [`Self::resolve_token_with_ip`] it also
    /// honors playback-only tokens. The router restricts those tokens to the playback allow-list
    /// before any handler runs.
    pub async fn resolve_emby_route_token_with_ip(
        &self,
        token: &str,
        client_ip: Option<IpAddr>,
    ) -> Result<Option<UserRecord>, EmbyAuthError> {
        self.resolve_token_scoped(token, client_ip, true).await
    }

    /// Resolves a request principal and evaluates every access rule for this request's address.
    ///
    /// The credential lookup and the rule snapshot are cached for a few seconds; the rules
    /// themselves are re-evaluated here on every call, because the address belongs to the request
    /// and not to the cached entry.
    async fn resolve_token_scoped(
        &self,
        token: &str,
        client_ip: Option<IpAddr>,
        allow_playback_scope: bool,
    ) -> Result<Option<UserRecord>, EmbyAuthError> {
        let is_playback_token = is_playback_token(token);
        if is_playback_token && !allow_playback_scope {
            return Ok(None);
        }
        let token_hash = hash_token(token);
        let Some(principal) = self
            .token_principal(&token_hash, allow_playback_scope)
            .await?
        else {
            // Unknown tokens must not reach the activity UPDATE below; otherwise any random
            // string would still open a write transaction on every request.
            return Ok(None);
        };
        let user_id = principal.user.id.to_string();
        // Both checks keep the baseline query shape: the IP rules are counted on their own (with
        // `MAX + 1` rows so an over-limit rule set fails closed) and the device/client rules are
        // matched by one EXISTS query. A shared rule snapshot ordered by rule type would truncate
        // the `IP_CIDR` rows first and let a blocked client through.
        if self.is_user_ip_blacklisted(&user_id, client_ip).await? {
            self.database.revoke_access_token(&token_hash).await?;
            return Ok(None);
        }
        if self
            .user_access_blacklist_matches(&user_id, &principal.device)
            .await?
        {
            self.database.revoke_access_token(&token_hash).await?;
            return Ok(None);
        }
        // A playback token keeps the expiry it was issued with; it is never refreshed by use.
        if !is_playback_token && self.should_touch_access_token(&token_hash) {
            self.database.touch_access_token(&token_hash).await?;
            self.mark_access_token_touched(&token_hash);
        }
        Ok(Some(principal.user.clone()))
    }

    /// Reads and caches the credential, its user and its device identity; the rules are read
    /// separately on every request.
    async fn token_principal(
        &self,
        token_hash: &[u8],
        allow_playback_scope: bool,
    ) -> Result<Option<Arc<CachedEmbyPrincipal>>, EmbyAuthError> {
        let epochs = self.database.auth_cache();
        let cache_key = principal_cache_key(token_hash, allow_playback_scope);
        if let Some(entry) = self.principal_cache.lookup(&cache_key, Instant::now()) {
            let principal = entry.value();
            if epochs.principal_is_current(principal.snapshot) {
                return Ok(Some(principal));
            }
        }
        // Capture the epochs before reading: if anything invalidates them while the queries below
        // run, the snapshot is stale and the value is not cached.
        let snapshot = epochs.principal_snapshot();
        let Some(stored) = self
            .database
            .find_route_token_principal(token_hash, allow_playback_scope)
            .await?
        else {
            return Ok(None);
        };
        if stored.user.is_disabled {
            return Ok(None);
        }
        let user = user_record(stored.user)?;
        let device = EmbyDeviceInfo {
            client: stored.device.client_name,
            device: stored.device.device_name,
            device_id: stored.device.device_id,
            version: stored.device.client_version,
            user_id: None,
        };
        if epochs.principal_is_current(snapshot) {
            self.principal_cache.store(
                cache_key,
                CachedEmbyPrincipal {
                    user: user.clone(),
                    device: device.clone(),
                    snapshot,
                },
                Instant::now(),
            );
        }
        Ok(Some(Arc::new(CachedEmbyPrincipal {
            user,
            device,
            snapshot,
        })))
    }

    /// Issues a short-lived, playback-only token bound to one Web playback session. It expires no
    /// later than `expires_at` (and never more than [`PLAYBACK_TOKEN_MAX_TTL_SECONDS`] from now).
    pub async fn issue_playback_token(
        &self,
        user: &UserRecord,
        item_public_id: &str,
        playback_session_id: &str,
        device: &EmbyDeviceInfo,
        expires_at: i64,
        client_ip: Option<IpAddr>,
    ) -> Result<Option<String>, EmbyAuthError> {
        // The item id travels inside the token (the hash covers it), so the router can confine
        // the token to one item without a database lookup.
        if item_public_id.is_empty()
            || !item_public_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Ok(None);
        }
        let user_id = user.id.to_string();
        if self.user_access_blacklist_matches(&user_id, device).await?
            || self.is_user_ip_blacklisted(&user_id, client_ip).await?
        {
            return Ok(None);
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| {
                i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
            });
        let expires_at = expires_at.min(now.saturating_add(PLAYBACK_TOKEN_MAX_TTL_SECONDS));
        let token = format!(
            "{PLAYBACK_TOKEN_PREFIX}{item_public_id}_{}",
            generate_token()?
        );
        let token_id = UserId::new().to_string();
        let token_hash = hash_token(&token);
        self.database
            .create_playback_access_token(NewPlaybackAccessToken {
                id: &token_id,
                token_hash: &token_hash,
                user_id: &user_id,
                device_id: &device.device_id,
                client_name: &device.client,
                device_name: &device.device,
                client_version: &device.version,
                playback_session_id,
                expires_at,
            })
            .await?;
        Ok(Some(token))
    }

    /// Revokes every playback token issued for a playback session.
    pub async fn revoke_playback_tokens(
        &self,
        playback_session_id: &str,
    ) -> Result<u64, EmbyAuthError> {
        Ok(self
            .database
            .revoke_playback_access_tokens(playback_session_id)
            .await?)
    }

    fn should_touch_access_token(&self, token_hash: &[u8]) -> bool {
        let now = Instant::now();
        self.with_activity_cache(|cache| cache.should_touch(token_hash, now))
    }

    fn mark_access_token_touched(&self, token_hash: &[u8]) {
        let now = Instant::now();
        self.with_activity_cache(|cache| cache.mark_touched(token_hash, now));
    }

    pub(crate) fn device_access_refresh_due(&self, token: &str) -> bool {
        let token_hash = hash_token(token);
        let now = Instant::now();
        self.with_activity_cache(|cache| cache.device_refresh_due(&token_hash, now))
    }

    pub(crate) fn mark_device_access_refreshed(&self, token: &str) {
        let token_hash = hash_token(token);
        let now = Instant::now();
        self.with_activity_cache(|cache| cache.mark_device_refreshed(&token_hash, now));
    }

    fn with_activity_cache<T>(&self, operation: impl FnOnce(&mut EmbyActivityCache) -> T) -> T {
        let mut cache = self
            .activity_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        operation(&mut cache)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmbyLoginSessionSummary {
    pub id: String,
    pub device_id: String,
    pub device_name: String,
    pub device_type: Option<String>,
    pub client_name: String,
    pub client_version: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_seen_at: Option<i64>,
}

impl From<StoredEmbyLoginSessionSummary> for EmbyLoginSessionSummary {
    fn from(session: StoredEmbyLoginSessionSummary) -> Self {
        Self {
            id: session.id,
            device_id: session.device_id,
            device_name: session.device_name,
            device_type: session.device_type,
            client_name: session.client_name,
            client_version: session.client_version,
            created_at: session.created_at,
            updated_at: session.updated_at,
            last_seen_at: session.last_seen_at,
        }
    }
}

fn user_record(stored: crate::storage::StoredUser) -> Result<UserRecord, EmbyAuthError> {
    let id = stored
        .id
        .parse()
        .map_err(|error: uuid::Error| EmbyAuthError::InvalidUserId(error.to_string()))?;
    Ok(UserRecord {
        id,
        username_normalized: stored.username_normalized,
        display_name: stored.display_name,
        has_password: stored.has_password,
        is_disabled: stored.is_disabled,
        is_admin: stored.is_admin,
        can_manage_server: stored.can_manage_server,
        can_remote_access: stored.can_remote_access,
        can_download: stored.can_download,
        last_login_at: stored.last_login_at,
        last_activity_at: stored.last_activity_at,
    })
}

#[derive(Clone, Debug, Default)]
pub struct EmbyDeviceInfo {
    pub client: String,
    pub device: String,
    pub device_id: String,
    pub version: String,
    pub user_id: Option<String>,
}

impl EmbyDeviceInfo {
    pub fn parse(header: &str) -> Self {
        let mut info = Self::default();
        let Some((_, values)) = header.split_once(' ') else {
            return info;
        };
        for part in values.split(',') {
            let Some((key, value)) = part.trim().split_once('=') else {
                continue;
            };
            let value = value.trim().trim_matches('"').to_owned();
            match key.trim() {
                "Client" => info.client = value,
                "Device" => info.device = value,
                "DeviceId" => info.device_id = value,
                "Version" => info.version = value,
                "UserId" => info.user_id = Some(value),
                _ => {}
            }
        }
        info
    }
}

#[derive(Clone, Debug)]
pub struct EmbyAuthResult {
    pub token: String,
    pub session_id: String,
    pub user: UserRecord,
    pub device: EmbyDeviceInfo,
}

/// Prefix that marks a playback-only token so the router can restrict it without a lookup.
pub const PLAYBACK_TOKEN_PREFIX: &str = "luxpb_";
const PLAYBACK_TOKEN_MAX_TTL_SECONDS: i64 = 12 * 60 * 60;

/// Whether `token` has the shape of a playback-only token (it may still be unknown or expired).
pub fn is_playback_token(token: &str) -> bool {
    token.starts_with(PLAYBACK_TOKEN_PREFIX)
}

/// The item id a playback-only token is bound to (`luxpb_{item}_{random}`).
pub fn playback_token_item(token: &str) -> Option<&str> {
    let (item, _) = token.strip_prefix(PLAYBACK_TOKEN_PREFIX)?.split_once('_')?;
    (!item.is_empty()).then_some(item)
}

fn generate_token() -> Result<String, EmbyAuthError> {
    let mut bytes = [0_u8; 32];
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|error| EmbyAuthError::TokenGeneration(error.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// The cache key separates the two resolver scopes: a playback-only token resolves differently
/// depending on whether the caller allows playback credentials at all.
fn principal_cache_key(token_hash: &[u8], allow_playback_scope: bool) -> Vec<u8> {
    let mut key = Vec::with_capacity(token_hash.len() + 1);
    key.extend_from_slice(token_hash);
    key.push(u8::from(allow_playback_scope));
    key
}

#[derive(Debug)]
pub enum EmbyAuthError {
    AccessRulesUnavailable,
    InvalidUserId(String),
    Password(PasswordError),
    Storage(StorageError),
    TokenGeneration(String),
    UserStore(UserStoreError),
}

impl From<PasswordError> for EmbyAuthError {
    fn from(error: PasswordError) -> Self {
        Self::Password(error)
    }
}

impl From<StorageError> for EmbyAuthError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<UserStoreError> for EmbyAuthError {
    fn from(error: UserStoreError) -> Self {
        Self::UserStore(error)
    }
}

impl std::fmt::Display for EmbyAuthError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AccessRulesUnavailable => formatter.write_str("access rules are unavailable"),
            Self::InvalidUserId(error) => write!(formatter, "stored user ID is invalid: {error}"),
            Self::Password(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
            Self::TokenGeneration(error) => write!(formatter, "token generation failed: {error}"),
            Self::UserStore(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for EmbyAuthError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Password(error) => Some(error),
            Self::Storage(error) => Some(error),
            Self::UserStore(error) => Some(error),
            Self::AccessRulesUnavailable | Self::InvalidUserId(_) | Self::TokenGeneration(_) => {
                None
            }
        }
    }
}

#[cfg(test)]
mod route_token_tests {
    use super::{EmbyAuthService, EmbyDeviceInfo};
    use crate::{
        auth::users::UserStore,
        storage::{Database, NewUserAccessBlacklistRule, UpdateUser},
    };

    fn device() -> EmbyDeviceInfo {
        EmbyDeviceInfo {
            client: "Emby Client".to_owned(),
            device: "Living Room".to_owned(),
            device_id: "device-cache".to_owned(),
            version: "1.0".to_owned(),
            user_id: None,
        }
    }

    /// An Emby login for one regular user, with the token the client would send.
    async fn fixture()
    -> Result<(Database, EmbyAuthService, String, String), Box<dyn std::error::Error>> {
        let (temp, database) = crate::storage::test_database().await?;
        // The connection keeps the database file open for the whole test.
        Box::leak(Box::new(temp));
        let user = UserStore::new(database.clone())?
            .create_user("emby-cache", "Emby Cache", "emby password", false)
            .await?;
        let service = EmbyAuthService::new(database.clone())?;
        let session = service
            .authenticate("emby-cache", "emby password", &device())
            .await?
            .ok_or("login failed")?;
        Ok((database, service, session.token, user.id.to_string()))
    }

    async fn add_rule(database: &Database, user_id: Option<&str>, rule_type: &str, value: &str) {
        let id = match user_id {
            Some(_) => format!("route-rule-{rule_type}-{value}"),
            None => format!("route-global-rule-{rule_type}-{value}"),
        };
        database
            .insert_user_access_blacklist_rule(NewUserAccessBlacklistRule {
                id: Box::leak(id.into_boxed_str()),
                user_id,
                rule_type,
                match_value: value,
                is_enabled: true,
                created_by: user_id,
                created_at: 1,
            })
            .await
            .expect("rule insert");
    }

    #[tokio::test]
    async fn token_resolution_merges_the_credential_and_device_lookups()
    -> Result<(), Box<dyn std::error::Error>> {
        let (database, service, token, _user_id) = fixture().await?;
        database.reset_query_count();
        assert!(service.resolve_token(&token).await?.is_some());
        assert_eq!(
            database.query_count(),
            4,
            "one JOIN for the credential and device, one IP-rule read, one device/client match, \
             one throttled activity touch"
        );
        Ok(())
    }

    #[tokio::test]
    async fn token_resolution_is_cached_and_revocation_wins()
    -> Result<(), Box<dyn std::error::Error>> {
        let (database, service, token, _user_id) = fixture().await?;
        assert!(service.resolve_token(&token).await?.is_some());
        database.reset_query_count();
        assert!(service.resolve_token(&token).await?.is_some());
        assert_eq!(
            database.query_count(),
            2,
            "the credential is cached; the two rule checks are still read per request"
        );

        service.logout(&token).await?;
        database.reset_query_count();
        assert!(
            service.resolve_token(&token).await?.is_none(),
            "a revoked token must fail on the very next request"
        );
        assert!(database.query_count() > 0);
        Ok(())
    }

    #[tokio::test]
    async fn device_and_client_rules_apply_to_the_very_next_request()
    -> Result<(), Box<dyn std::error::Error>> {
        let (database, service, token, user_id) = fixture().await?;
        assert!(service.resolve_token(&token).await?.is_some());
        database.reset_query_count();
        assert!(service.resolve_token(&token).await?.is_some());
        assert_eq!(database.query_count(), 2);

        add_rule(&database, Some(&user_id), "DEVICE", "device-cache").await;
        database.reset_query_count();
        assert!(
            service.resolve_token(&token).await?.is_none(),
            "a device rule must apply to the very next request"
        );
        assert!(database.query_count() > 0);
        Ok(())
    }

    #[tokio::test]
    async fn many_non_ip_rules_do_not_hide_an_ip_rule() -> Result<(), Box<dyn std::error::Error>> {
        let (database, service, token, user_id) = fixture().await?;
        // More than the 400-row snapshot the resolver used to read: with a shared, rule-type
        // ordered snapshot these rows truncate the IP_CIDR row and the client would pass.
        for index in 0..401 {
            add_rule(
                &database,
                Some(&user_id),
                "DEVICE",
                &format!("other-device-{index}"),
            )
            .await;
        }
        add_rule(&database, Some(&user_id), "IP_CIDR", "10.9.0.0/16").await;

        assert!(
            service
                .resolve_token_with_ip(&token, Some("10.9.1.2".parse()?))
                .await?
                .is_none(),
            "an IP rule must still block when the device/client rules overflow a snapshot"
        );
        Ok(())
    }

    #[tokio::test]
    async fn too_many_ip_rules_fail_closed() -> Result<(), Box<dyn std::error::Error>> {
        let (database, service, token, user_id) = fixture().await?;
        for index in
            0..=crate::application::user_access_rules::MAX_ACTIVE_IP_BLACKLIST_RULES_PER_REQUEST
        {
            add_rule(
                &database,
                Some(&user_id),
                "IP_CIDR",
                &format!("10.10.{}.0/24", index % 250),
            )
            .await;
        }

        assert!(
            matches!(
                service.resolve_token(&token).await,
                Err(crate::auth::emby::EmbyAuthError::AccessRulesUnavailable)
            ),
            "an over-limit IP rule set must deny instead of evaluating a truncated list"
        );
        Ok(())
    }

    #[tokio::test]
    async fn token_resolution_cache_is_invalidated_by_playback_token_revocation()
    -> Result<(), Box<dyn std::error::Error>> {
        let (database, service, _token, user_id) = fixture().await?;
        let user = service.user_by_id(&user_id).await?.ok_or("user missing")?;
        let playback_token = service
            .issue_playback_token(&user, "100", "session-cache", &device(), i64::MAX, None)
            .await?
            .ok_or("playback token was not issued")?;
        assert!(
            service
                .resolve_emby_route_token_with_ip(&playback_token, None)
                .await?
                .is_some()
        );
        database.reset_query_count();
        assert!(
            service
                .resolve_emby_route_token_with_ip(&playback_token, None)
                .await?
                .is_some()
        );
        assert_eq!(database.query_count(), 2);

        assert_eq!(
            database
                .revoke_playback_access_tokens("session-cache")
                .await?,
            1
        );
        assert!(
            service
                .resolve_emby_route_token_with_ip(&playback_token, None)
                .await?
                .is_none(),
            "a revoked playback token must fail on the very next request"
        );
        Ok(())
    }

    #[tokio::test]
    async fn global_rules_apply_to_the_very_next_request() -> Result<(), Box<dyn std::error::Error>>
    {
        let (database, service, token, _user_id) = fixture().await?;
        assert!(service.resolve_token(&token).await?.is_some());
        database.reset_query_count();
        assert!(service.resolve_token(&token).await?.is_some());
        assert_eq!(database.query_count(), 2);

        add_rule(&database, None, "CLIENT", "emby client").await;
        assert!(
            service.resolve_token(&token).await?.is_none(),
            "a global client rule applies to every user on the very next request"
        );
        Ok(())
    }

    #[tokio::test]
    async fn token_resolution_cache_is_invalidated_by_account_changes()
    -> Result<(), Box<dyn std::error::Error>> {
        let (database, service, token, user_id) = fixture().await?;
        assert!(service.resolve_token(&token).await?.is_some());
        database.reset_query_count();
        assert!(service.resolve_token(&token).await?.is_some());
        assert_eq!(database.query_count(), 2);

        database
            .update_user(
                &user_id,
                UpdateUser {
                    display_name: None,
                    password_hash: None,
                    has_password: None,
                    is_disabled: Some(true),
                    is_admin: None,
                    can_manage_server: None,
                    can_remote_access: None,
                    can_download: None,
                },
            )
            .await?;
        database.reset_query_count();
        assert!(
            service.resolve_token(&token).await?.is_none(),
            "disabling the account must apply to the very next request"
        );
        assert!(database.query_count() > 0);
        Ok(())
    }

    #[tokio::test]
    async fn token_resolution_cache_is_invalidated_by_user_deletion()
    -> Result<(), Box<dyn std::error::Error>> {
        let (database, service, token, user_id) = fixture().await?;
        assert!(service.resolve_token(&token).await?.is_some());
        database.reset_query_count();
        assert!(service.resolve_token(&token).await?.is_some());
        assert_eq!(database.query_count(), 2);

        assert!(database.delete_user(&user_id).await?);
        assert!(service.resolve_token(&token).await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn cached_rules_still_evaluate_each_request_address()
    -> Result<(), Box<dyn std::error::Error>> {
        let (database, service, token, user_id) = fixture().await?;
        add_rule(&database, Some(&user_id), "IP_CIDR", "10.0.0.1/32").await;
        assert!(
            service
                .resolve_token_with_ip(&token, Some("10.0.0.2".parse()?))
                .await?
                .is_some()
        );
        database.reset_query_count();
        assert!(
            service
                .resolve_token_with_ip(&token, Some("10.0.0.2".parse()?))
                .await?
                .is_some()
        );
        assert_eq!(database.query_count(), 2);

        assert!(
            service
                .resolve_token_with_ip(&token, Some("10.0.0.1".parse()?))
                .await?
                .is_none(),
            "the cached rule list must be evaluated against this request's address"
        );
        Ok(())
    }
}

#[cfg(test)]
mod activity_throttle_tests {
    use super::*;

    #[test]
    fn token_touch_is_throttled_per_token() {
        let mut cache = EmbyActivityCache::default();
        let now = Instant::now();

        assert!(cache.should_touch(b"token-a", now));
        cache.mark_touched(b"token-a", now);
        assert!(!cache.should_touch(b"token-a", now + Duration::from_secs(1)));
        assert!(cache.should_touch(b"token-a", now + ACCESS_TOKEN_TOUCH_INTERVAL));
        assert!(cache.should_touch(b"token-b", now));
    }

    #[test]
    fn device_refresh_is_throttled_per_token() {
        let mut cache = EmbyActivityCache::default();
        let now = Instant::now();

        assert!(cache.device_refresh_due(b"token-a", now));
        cache.mark_device_refreshed(b"token-a", now);
        assert!(!cache.device_refresh_due(b"token-a", now + Duration::from_secs(30)));
        assert!(cache.device_refresh_due(b"token-a", now + DEVICE_ACCESS_REFRESH_INTERVAL));
    }

    #[test]
    fn activity_cache_prunes_old_entries_before_clearing() {
        let mut cache = EmbyActivityCache::default();
        let now = Instant::now();
        let old = now
            .checked_sub(ACTIVITY_CACHE_RETENTION + Duration::from_secs(1))
            .expect("test instant is older than the retention window");
        cache.token_touches.insert(b"token-old".to_vec(), old);
        for index in 0..(ACTIVITY_CACHE_MAX_ENTRIES - 1) {
            cache
                .token_touches
                .insert(format!("token-{index}").into_bytes(), now);
        }
        cache.mark_touched(b"token-new", now);
        assert_eq!(cache.token_touches.len(), ACTIVITY_CACHE_MAX_ENTRIES);
        assert!(!cache.token_touches.contains_key(b"token-old".as_slice()));
    }
}
