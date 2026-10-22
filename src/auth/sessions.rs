use std::net::IpAddr;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::{
    application::user_access_rules::{
        MAX_ACTIVE_IP_BLACKLIST_RULES_PER_REQUEST, ip_blacklist_rules_match_address,
    },
    auth::{
        password::PasswordError,
        users::{UserRecord, UserStore, UserStoreError},
    },
    domain::ids::UserId,
    storage::{Database, StorageError, UserAccessBlacklistIpRuleQuery},
};

const SESSION_LIFETIME_SECONDS: i64 = 30 * 24 * 60 * 60;

/// Upper bound for one access-rule snapshot; the per-user rule list is tiny in practice.
const ACCESS_RULE_SNAPSHOT_LIMIT: i64 = 400;

fn authenticated_session_from_stored(
    stored: crate::storage::StoredWebSession,
) -> Result<AuthenticatedSession, WebAuthError> {
    let id = stored
        .user_id
        .parse()
        .map_err(|error: uuid::Error| WebAuthError::InvalidUserId(error.to_string()))?;
    Ok(AuthenticatedSession {
        user: UserRecord {
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
        },
        csrf_token_hash: stored.csrf_token_hash,
        device_id: stored.device_id,
    })
}

/// Outcome of resolving a web request principal.
pub(crate) enum PrincipalResolution {
    Unauthenticated,
    /// A device or client rule blocks this session.
    AccessDenied,
    Session(AuthenticatedSession),
}

#[derive(Clone)]
pub struct WebAuthService {
    database: Database,
    users: UserStore,
}

impl WebAuthService {
    pub fn new(database: Database) -> Result<Self, PasswordError> {
        Ok(Self {
            users: UserStore::new(database.clone())?,
            database,
        })
    }

    pub async fn login(
        &self,
        username: &str,
        password: &str,
    ) -> Result<Option<LoginSession>, WebAuthError> {
        self.login_with_device(username, password, None).await
    }

    pub async fn login_with_device(
        &self,
        username: &str,
        password: &str,
        device_id: Option<&str>,
    ) -> Result<Option<LoginSession>, WebAuthError> {
        self.login_with_device_and_ip(username, password, device_id, None)
            .await
    }

    pub async fn login_with_device_and_ip(
        &self,
        username: &str,
        password: &str,
        device_id: Option<&str>,
        client_ip: Option<IpAddr>,
    ) -> Result<Option<LoginSession>, WebAuthError> {
        let Some(user) = self.users.authenticate(username, password).await? else {
            return Ok(None);
        };
        if self
            .is_user_access_blacklisted(&user.id.to_string(), device_id)
            .await?
        {
            return Err(WebAuthError::AccessDenied);
        }
        if self
            .is_user_ip_blacklisted(&user.id.to_string(), client_ip)
            .await?
        {
            return Err(WebAuthError::AccessDenied);
        }
        let session_token = generate_token()?;
        let csrf_token = generate_token()?;
        let session_id = UserId::new();
        let session_id = session_id.to_string();
        let user_id = user.id.to_string();
        let session_token_hash = hash_token(&session_token);
        let csrf_token_hash = hash_token(&csrf_token);
        match device_id {
            Some(device_id) => {
                self.database
                    .create_web_session_with_device(
                        &session_id,
                        &user_id,
                        &session_token_hash,
                        &csrf_token_hash,
                        Some(device_id),
                        SESSION_LIFETIME_SECONDS,
                    )
                    .await?;
            }
            None => {
                self.database
                    .create_web_session(
                        &session_id,
                        &user_id,
                        &session_token_hash,
                        &csrf_token_hash,
                        SESSION_LIFETIME_SECONDS,
                    )
                    .await?;
            }
        }

        Ok(Some(LoginSession {
            session_token,
            csrf_token,
            user,
        }))
    }

    pub async fn is_user_access_blacklisted(
        &self,
        user_id: &str,
        device_id: Option<&str>,
    ) -> Result<bool, WebAuthError> {
        Ok(self
            .database
            .user_access_blacklist_matches(crate::storage::UserAccessBlacklistRuleMatchQuery {
                user_id,
                device_id,
                client_name: Some("lux web"),
            })
            .await?)
    }

    pub(crate) async fn find_user_id_by_username(
        &self,
        username: &str,
    ) -> Result<Option<String>, WebAuthError> {
        Ok(self
            .users
            .find_by_username(username)
            .await?
            .map(|user| user.id.to_string()))
    }

    pub async fn change_password(
        &self,
        user_id: &UserId,
        current_password: &str,
        new_password: &str,
        keep_session_token_hash: Option<&[u8]>,
    ) -> Result<bool, WebAuthError> {
        Ok(self
            .users
            .change_password(
                user_id,
                current_password,
                new_password,
                keep_session_token_hash,
            )
            .await?)
    }

    /// Hash of a raw web session token, used to keep the caller's session on password change.
    pub(crate) fn session_token_hash(token: &str) -> Vec<u8> {
        hash_token(token)
    }

    pub async fn resolve(
        &self,
        session_token: &str,
    ) -> Result<Option<AuthenticatedSession>, WebAuthError> {
        self.resolve_internal(session_token, None, false).await
    }

    pub async fn resolve_with_ip(
        &self,
        session_token: &str,
        client_ip: Option<IpAddr>,
    ) -> Result<Option<AuthenticatedSession>, WebAuthError> {
        self.resolve_internal(session_token, client_ip, true).await
    }

    async fn resolve_internal(
        &self,
        session_token: &str,
        client_ip: Option<IpAddr>,
        enforce_ip_rules: bool,
    ) -> Result<Option<AuthenticatedSession>, WebAuthError> {
        let Some(stored) = self
            .database
            .find_web_session(&hash_token(session_token))
            .await?
        else {
            return Ok(None);
        };
        if stored.is_disabled {
            return Ok(None);
        }
        if enforce_ip_rules
            && self
                .is_user_ip_blacklisted(&stored.user_id, client_ip)
                .await?
        {
            self.logout(session_token).await?;
            return Ok(None);
        }
        Ok(Some(authenticated_session_from_stored(stored)?))
    }

    /// Resolves a request principal and evaluates every access rule in a single rules query.
    ///
    /// IP rules log the session out (as before); device and client rules deny the request but
    /// keep the session, which is what the admin UI shows as a blocked client.
    pub(crate) async fn resolve_principal(
        &self,
        session_token: &str,
        client_ip: Option<IpAddr>,
    ) -> Result<PrincipalResolution, WebAuthError> {
        let Some(stored) = self
            .database
            .find_web_session(&hash_token(session_token))
            .await?
        else {
            return Ok(PrincipalResolution::Unauthenticated);
        };
        if stored.is_disabled {
            return Ok(PrincipalResolution::Unauthenticated);
        }
        let rules = self
            .database
            .user_access_blacklist_rules_for_user(&stored.user_id, ACCESS_RULE_SNAPSHOT_LIMIT)
            .await?;
        let ip_rules = rules
            .iter()
            .filter(|(rule_type, _)| rule_type == "IP_CIDR")
            .map(|(_, value)| value.clone())
            .collect::<Vec<_>>();
        if ip_rules.len() > MAX_ACTIVE_IP_BLACKLIST_RULES_PER_REQUEST {
            return Err(WebAuthError::AccessRulesUnavailable);
        }
        if !ip_rules.is_empty() {
            let blocked = match client_ip {
                Some(client_ip) => ip_blacklist_rules_match_address(client_ip, &ip_rules)
                    .map_err(|_| WebAuthError::AccessRulesUnavailable)?,
                // Without a resolvable client address the rule cannot be proven satisfied.
                None => true,
            };
            if blocked {
                self.logout(session_token).await?;
                return Ok(PrincipalResolution::Unauthenticated);
            }
        }
        let device_id = stored.device_id.as_deref();
        let denied = rules
            .iter()
            .any(|(rule_type, value)| match rule_type.as_str() {
                "DEVICE" => device_id == Some(value.as_str()),
                "CLIENT" => "lux web" == value,
                _ => false,
            });
        if denied {
            return Ok(PrincipalResolution::AccessDenied);
        }
        Ok(PrincipalResolution::Session(
            authenticated_session_from_stored(stored)?,
        ))
    }

    async fn is_user_ip_blacklisted(
        &self,
        user_id: &str,
        client_ip: Option<IpAddr>,
    ) -> Result<bool, WebAuthError> {
        let rules = self
            .database
            .list_user_access_ip_blacklist_rule_values(UserAccessBlacklistIpRuleQuery {
                user_id,
                limit: 201,
            })
            .await?;
        if rules.len() > MAX_ACTIVE_IP_BLACKLIST_RULES_PER_REQUEST {
            return Err(WebAuthError::AccessRulesUnavailable);
        }
        if rules.is_empty() {
            return Ok(false);
        }
        let Some(client_ip) = client_ip else {
            return Ok(true);
        };
        ip_blacklist_rules_match_address(client_ip, &rules)
            .map_err(|_| WebAuthError::AccessRulesUnavailable)
    }

    pub async fn logout(&self, session_token: &str) -> Result<(), WebAuthError> {
        self.database
            .revoke_web_session(&hash_token(session_token))
            .await?;
        Ok(())
    }

    pub async fn list_sessions(
        &self,
        user_id: &UserId,
        current_session_token: &str,
    ) -> Result<Vec<WebSessionSummary>, WebAuthError> {
        Ok(self
            .database
            .list_web_session_summaries(&user_id.to_string(), &hash_token(current_session_token))
            .await?
            .into_iter()
            .map(|session| WebSessionSummary {
                id: session.id,
                created_at: session.created_at,
                updated_at: session.updated_at,
                expires_at: session.expires_at,
                last_seen_at: session.last_seen_at,
                is_current: session.is_current,
            })
            .collect())
    }

    pub async fn list_user_sessions_for_admin(
        &self,
        user_id: &str,
        current_session_token: Option<&str>,
        page_size: i64,
        offset: i64,
    ) -> Result<(Vec<WebSessionSummary>, i64), WebAuthError> {
        let current_token_hash = current_session_token.map(hash_token).unwrap_or_default();
        let (sessions, total_count) = self
            .database
            .list_web_session_summaries_page(user_id, &current_token_hash, page_size, offset)
            .await?;
        Ok((
            sessions
                .into_iter()
                .map(|session| WebSessionSummary {
                    id: session.id,
                    created_at: session.created_at,
                    updated_at: session.updated_at,
                    expires_at: session.expires_at,
                    last_seen_at: session.last_seen_at,
                    is_current: session.is_current,
                })
                .collect(),
            total_count,
        ))
    }

    pub async fn is_current_session_for_admin(
        &self,
        user_id: &str,
        session_id: &str,
        current_session_token: &str,
    ) -> Result<bool, WebAuthError> {
        Ok(self
            .database
            .is_active_web_session_token(user_id, session_id, &hash_token(current_session_token))
            .await?)
    }

    pub async fn revoke_session(
        &self,
        user_id: &UserId,
        session_id: &str,
    ) -> Result<bool, WebAuthError> {
        Ok(self
            .database
            .revoke_web_session_by_id(&user_id.to_string(), session_id)
            .await?)
    }

    pub fn verify_csrf(&self, session: &AuthenticatedSession, csrf_token: &str) -> bool {
        hash_token(csrf_token)
            .as_slice()
            .ct_eq(&session.csrf_token_hash)
            .into()
    }
}

#[derive(Clone, Debug)]
pub struct LoginSession {
    pub session_token: String,
    pub csrf_token: String,
    pub user: UserRecord,
}

#[derive(Clone, Debug)]
pub struct WebSessionSummary {
    pub id: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub expires_at: i64,
    pub last_seen_at: Option<i64>,
    pub is_current: bool,
}

#[derive(Clone, Debug)]
pub struct AuthenticatedSession {
    pub user: UserRecord,
    csrf_token_hash: Vec<u8>,
    pub(crate) device_id: Option<String>,
}

fn generate_token() -> Result<String, WebAuthError> {
    let mut bytes = [0_u8; 32];
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|error| WebAuthError::TokenGeneration(error.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

#[derive(Debug)]
pub enum WebAuthError {
    AccessDenied,
    AccessRulesUnavailable,
    InvalidUserId(String),
    Password(PasswordError),
    Storage(StorageError),
    TokenGeneration(String),
    UserStore(UserStoreError),
}

impl From<PasswordError> for WebAuthError {
    fn from(error: PasswordError) -> Self {
        Self::Password(error)
    }
}

impl From<StorageError> for WebAuthError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<UserStoreError> for WebAuthError {
    fn from(error: UserStoreError) -> Self {
        Self::UserStore(error)
    }
}

impl std::fmt::Display for WebAuthError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AccessDenied => formatter.write_str("access denied by account policy"),
            Self::AccessRulesUnavailable => formatter.write_str("access rules are unavailable"),
            Self::InvalidUserId(error) => write!(formatter, "stored user ID is invalid: {error}"),
            Self::Password(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
            Self::TokenGeneration(error) => write!(formatter, "token generation failed: {error}"),
            Self::UserStore(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for WebAuthError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Password(error) => Some(error),
            Self::Storage(error) => Some(error),
            Self::UserStore(error) => Some(error),
            Self::AccessDenied
            | Self::AccessRulesUnavailable
            | Self::InvalidUserId(_)
            | Self::TokenGeneration(_) => None,
        }
    }
}

#[cfg(test)]
mod principal_tests {
    use super::{PrincipalResolution, WebAuthService};
    use crate::{
        application::setup::SetupService,
        config::Config,
        storage::{Database, NewUserAccessBlacklistRule},
    };

    async fn fixture()
    -> Result<(Database, WebAuthService, String, String), Box<dyn std::error::Error>> {
        let temp = Box::leak(Box::new(tempfile::tempdir()?));
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp.path().join("config"),
        })
        .await?;
        let admin = SetupService::new(database.clone())?
            .complete("Admin", "Admin", "admin password")
            .await?;
        let service = WebAuthService::new(database.clone())?;
        let session = service
            .login_with_device("admin", "admin password", Some("device-1"))
            .await?
            .ok_or("login failed")?;
        Ok((
            database,
            service,
            session.session_token,
            admin.id.to_string(),
        ))
    }

    async fn add_rule(database: &Database, user_id: &str, rule_type: &str, value: &str) {
        database
            .insert_user_access_blacklist_rule(NewUserAccessBlacklistRule {
                id: Box::leak(format!("rule-{rule_type}-{value}").into_boxed_str()),
                user_id: Some(user_id),
                rule_type,
                match_value: value,
                is_enabled: true,
                created_by: Some(user_id),
                created_at: 1,
            })
            .await
            .expect("rule insert");
    }

    #[tokio::test]
    async fn principal_resolution_evaluates_device_and_client_rules()
    -> Result<(), Box<dyn std::error::Error>> {
        let (database, service, token, user_id) = fixture().await?;
        let ip = Some("127.0.0.1".parse().expect("loopback"));

        assert!(matches!(
            service.resolve_principal(&token, ip).await?,
            PrincipalResolution::Session(_)
        ));

        add_rule(&database, &user_id, "CLIENT", "lux web").await;
        assert!(matches!(
            service.resolve_principal(&token, ip).await?,
            PrincipalResolution::AccessDenied
        ));

        database
            .set_user_access_blacklist_rule_enabled(
                crate::storage::UserAccessBlacklistRuleScope::User(&user_id),
                "rule-CLIENT-lux web",
                false,
            )
            .await?;
        add_rule(&database, &user_id, "DEVICE", "device-1").await;
        assert!(matches!(
            service.resolve_principal(&token, ip).await?,
            PrincipalResolution::AccessDenied
        ));
        Ok(())
    }

    #[tokio::test]
    async fn principal_resolution_logs_out_on_a_matching_ip_rule()
    -> Result<(), Box<dyn std::error::Error>> {
        let (database, service, token, user_id) = fixture().await?;
        add_rule(&database, &user_id, "IP_CIDR", "127.0.0.1/32").await;
        assert!(matches!(
            service
                .resolve_principal(&token, Some("127.0.0.1".parse()?))
                .await?,
            PrincipalResolution::Unauthenticated
        ));
        assert!(
            service
                .resolve_principal(&token, Some("127.0.0.1".parse()?))
                .await
                .is_ok()
        );
        Ok(())
    }
}
