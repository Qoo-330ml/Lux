use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::{Mailbox, header::ContentType},
};
use subtle::ConstantTimeEq;
use tokio::{sync::watch, task::JoinHandle};
use url::Url;

use crate::{
    application::mail::{
        MailChallengeSigner, MailError, MailSettings, SmtpCredentials, build_smtp_transport,
        hash_mail_challenge_token, read_mail_settings, read_smtp_credentials,
    },
    storage::{
        Database, MailChallengeKind, MailDeliveryErrorCode, MailOutboxTemplateKind, StorageError,
    },
};

const MAIL_QUEUE_POLL_INTERVAL: Duration = Duration::from_secs(5);
const MAIL_QUEUE_LEASE_SECONDS: i64 = 30;
const MAIL_RETRY_BASE_SECONDS: i64 = 30;
const MAX_MAIL_ATTEMPTS: i64 = 5;

pub struct MailDeliveryWorker {
    shutdown_sender: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl MailDeliveryWorker {
    pub fn start(database: Database, config_dir: PathBuf) -> Self {
        let (shutdown_sender, shutdown_receiver) = watch::channel(false);
        let task = tokio::spawn(run_mail_delivery_worker(
            database,
            config_dir,
            shutdown_receiver,
        ));
        Self {
            shutdown_sender,
            task,
        }
    }

    pub async fn shutdown(self) {
        self.shutdown_sender.send_replace(true);
        if self.task.await.is_err() {
            tracing::warn!(
                event_code = "MAIL_WORKER_TASK_FAILED",
                "mail worker stopped unexpectedly"
            );
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MailWorkerStep {
    Empty,
    Sent,
    Deferred,
    Failed,
}

#[derive(Clone, Copy)]
struct MailSendFailure;

type MailSendFuture<'a> = Pin<Box<dyn Future<Output = Result<(), MailSendFailure>> + Send + 'a>>;

trait MailSender: Send + Sync {
    fn send<'a>(&'a self, message: Message) -> MailSendFuture<'a>;
}

impl MailSender for AsyncSmtpTransport<Tokio1Executor> {
    fn send<'a>(&'a self, message: Message) -> MailSendFuture<'a> {
        Box::pin(async move {
            AsyncTransport::send(self, message)
                .await
                .map(|_| ())
                .map_err(|_| MailSendFailure)
        })
    }
}

enum PreparedMail {
    Ready(Box<PreparedMailMessage>),
    Retry(MailDeliveryErrorCode),
    Fail(MailDeliveryErrorCode),
}

struct PreparedMailMessage {
    settings: MailSettings,
    credentials: Option<SmtpCredentials>,
    message: Message,
}

async fn run_mail_delivery_worker(
    database: Database,
    config_dir: PathBuf,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        if *shutdown.borrow() {
            return;
        }

        match process_next_mail(
            &database,
            &config_dir,
            current_timestamp(),
            build_smtp_transport,
        )
        .await
        {
            Ok(MailWorkerStep::Sent | MailWorkerStep::Failed) => continue,
            Ok(MailWorkerStep::Empty | MailWorkerStep::Deferred) => {}
            Err(_) => {
                tracing::warn!(
                    event_code = "MAIL_QUEUE_PROCESSING_FAILED",
                    "mail queue processing failed"
                );
            }
        }

        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
            _ = tokio::time::sleep(MAIL_QUEUE_POLL_INTERVAL) => {}
        }
    }
}

async fn process_next_mail<F, S>(
    database: &Database,
    config_dir: &Path,
    now: i64,
    make_sender: F,
) -> Result<MailWorkerStep, StorageError>
where
    F: FnOnce(&MailSettings, Option<SmtpCredentials>) -> Result<S, MailError> + Send,
    S: MailSender + 'static,
{
    let Some(outbox) = database
        .claim_mail_outbox(now, MAIL_QUEUE_LEASE_SECONDS)
        .await?
    else {
        return Ok(MailWorkerStep::Empty);
    };

    let prepared = prepare_mail(database, config_dir, &outbox, now).await?;
    let (settings, credentials, message) = match prepared {
        PreparedMail::Retry(error_code) => {
            return record_failure(database, &outbox, error_code, now, false).await;
        }
        PreparedMail::Fail(error_code) => {
            return record_failure(database, &outbox, error_code, now, true).await;
        }
        PreparedMail::Ready(prepared) => {
            (prepared.settings, prepared.credentials, prepared.message)
        }
    };

    let sender = match make_sender(&settings, credentials) {
        Ok(sender) => sender,
        Err(_) => {
            return record_failure(
                database,
                &outbox,
                MailDeliveryErrorCode::SettingsMissing,
                now,
                false,
            )
            .await;
        }
    };

    if sender.send(message).await.is_err() {
        return record_failure(
            database,
            &outbox,
            MailDeliveryErrorCode::SmtpUnavailable,
            now,
            false,
        )
        .await;
    }

    let marked_sent = database
        .mark_mail_outbox_sent(&outbox.id, outbox.attempt_count, now)
        .await?;
    Ok(if marked_sent {
        MailWorkerStep::Sent
    } else {
        MailWorkerStep::Deferred
    })
}

async fn prepare_mail(
    database: &Database,
    config_dir: &Path,
    outbox: &crate::storage::StoredMailOutboxMessage,
    now: i64,
) -> Result<PreparedMail, StorageError> {
    let settings = match read_mail_settings(config_dir).await {
        Ok(Some(settings)) => settings,
        Ok(None) | Err(_) => {
            return Ok(PreparedMail::Retry(MailDeliveryErrorCode::SettingsMissing));
        }
    };
    let credentials = match read_smtp_credentials(config_dir).await {
        Ok(credentials) => credentials,
        Err(_) => {
            return Ok(PreparedMail::Retry(MailDeliveryErrorCode::SettingsMissing));
        }
    };
    let signer = match MailChallengeSigner::load_or_create(config_dir).await {
        Ok(signer) => signer,
        Err(_) => {
            return Ok(PreparedMail::Retry(MailDeliveryErrorCode::SettingsMissing));
        }
    };
    let Some(challenge) = database.get_mail_challenge(&outbox.reference_id).await? else {
        return Ok(PreparedMail::Fail(MailDeliveryErrorCode::ReferenceMissing));
    };
    let expected_kind = match outbox.template_kind {
        MailOutboxTemplateKind::Invitation => MailChallengeKind::Invitation,
        MailOutboxTemplateKind::EmailVerification => MailChallengeKind::EmailVerification,
    };
    if challenge.kind != expected_kind
        || challenge.recipient_email != outbox.recipient_email
        || challenge.expires_at <= now
        || challenge.created_at > now
        || challenge.consumed_at.is_some()
        || challenge.revoked_at.is_some()
    {
        return Ok(PreparedMail::Fail(MailDeliveryErrorCode::ReferenceMissing));
    }
    let Some(token) = signer.token_for(&challenge.id, challenge.kind, challenge.expires_at) else {
        return Ok(PreparedMail::Fail(MailDeliveryErrorCode::ReferenceMissing));
    };
    let token_hash = hash_mail_challenge_token(&token);
    if !bool::from(challenge.token_hash.as_slice().ct_eq(token_hash.as_slice())) {
        return Ok(PreparedMail::Fail(MailDeliveryErrorCode::ReferenceMissing));
    }
    let account_user_id = if outbox.template_kind == MailOutboxTemplateKind::EmailVerification {
        match database.find_user_by_id(&challenge.reference_id).await? {
            Some(user) if !user.is_disabled && user.deleted_at.is_none() => {
                Some(challenge.reference_id.as_str())
            }
            Some(_) => return Ok(PreparedMail::Fail(MailDeliveryErrorCode::ReferenceMissing)),
            None => None,
        }
    } else {
        None
    };
    let message = match render_message(&settings, outbox, &token, account_user_id) {
        Ok(message) => message,
        Err(error_code) => return Ok(PreparedMail::Fail(error_code)),
    };
    Ok(PreparedMail::Ready(Box::new(PreparedMailMessage {
        settings,
        credentials,
        message,
    })))
}

fn render_message(
    settings: &MailSettings,
    outbox: &crate::storage::StoredMailOutboxMessage,
    token: &str,
    account_user_id: Option<&str>,
) -> Result<Message, MailDeliveryErrorCode> {
    let (subject, body_intro) = match outbox.template_kind {
        MailOutboxTemplateKind::Invitation => {
            ("你收到加入 Lux 的邀请", "请打开以下链接完成邀请注册：")
        }
        MailOutboxTemplateKind::EmailVerification => {
            ("验证你的 Lux 邮箱", "请打开以下链接完成邮箱验证：")
        }
    };
    let link = match outbox.template_kind {
        MailOutboxTemplateKind::Invitation => {
            registration_link(&settings.site_url, &["register", "invitation"], token)
        }
        MailOutboxTemplateKind::EmailVerification => match account_user_id {
            Some(user_id) => account_email_verification_link(&settings.site_url, user_id, token),
            None => registration_link(&settings.site_url, &["register", "verify-email"], token),
        },
    }
    .ok_or(MailDeliveryErrorCode::TemplateMissing)?;
    let sender_address = settings
        .sender_email
        .parse()
        .map_err(|_| MailDeliveryErrorCode::SettingsMissing)?;
    let recipient_address = outbox
        .recipient_email
        .parse()
        .map_err(|_| MailDeliveryErrorCode::ReferenceMissing)?;
    let body = format!("{body_intro}\n\n{link}\n\n此链接在对应邮件凭证有效期内只能使用一次。\n");
    Message::builder()
        .from(Mailbox::new(
            Some(settings.sender_name.clone()),
            sender_address,
        ))
        .to(Mailbox::new(None, recipient_address))
        .subject(subject)
        .header(ContentType::TEXT_PLAIN)
        .body(body)
        .map_err(|_| MailDeliveryErrorCode::TemplateMissing)
}

fn registration_link(site_url: &str, path: &[&str; 2], token: &str) -> Option<Url> {
    let mut url = Url::parse(site_url).ok()?;
    {
        let mut path_segments = url.path_segments_mut().ok()?;
        path_segments.pop_if_empty().push(path[0]).push(path[1]);
    }
    url.set_fragment(Some(&format!("token={token}")));
    Some(url)
}

fn account_email_verification_link(site_url: &str, user_id: &str, token: &str) -> Option<Url> {
    let mut url = Url::parse(site_url).ok()?;
    {
        let mut path_segments = url.path_segments_mut().ok()?;
        path_segments.pop_if_empty().push("account").push(user_id);
    }
    url.set_fragment(Some(&format!("token={token}")));
    Some(url)
}

async fn record_failure(
    database: &Database,
    outbox: &crate::storage::StoredMailOutboxMessage,
    error_code: MailDeliveryErrorCode,
    now: i64,
    permanent: bool,
) -> Result<MailWorkerStep, StorageError> {
    if permanent || outbox.attempt_count >= MAX_MAIL_ATTEMPTS {
        let marked_failed = database
            .fail_mail_outbox(&outbox.id, outbox.attempt_count, error_code, now)
            .await?;
        return Ok(if marked_failed {
            MailWorkerStep::Failed
        } else {
            MailWorkerStep::Deferred
        });
    }
    database
        .defer_mail_outbox(
            &outbox.id,
            outbox.attempt_count,
            now.saturating_add(retry_delay_seconds(outbox.attempt_count)),
            error_code,
            now,
        )
        .await?;
    Ok(MailWorkerStep::Deferred)
}

fn retry_delay_seconds(attempt_count: i64) -> i64 {
    let exponent = attempt_count
        .saturating_sub(1)
        .clamp(0, MAX_MAIL_ATTEMPTS - 1) as u32;
    MAIL_RETRY_BASE_SECONDS.saturating_mul(1_i64 << exponent)
}

fn current_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64) as i64
}

#[cfg(test)]
mod tests {
    use std::{
        error::Error,
        path::Path,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use base64::{Engine, engine::general_purpose::STANDARD};
    use lettre::Message;
    use tempfile::TempDir;

    use super::{
        MAX_MAIL_ATTEMPTS, MailDeliveryWorker, MailSendFailure, MailSendFuture, MailSender,
        MailWorkerStep, process_next_mail,
    };
    use crate::{
        application::mail::{
            MailChallengeSigner, MailSettings, SmtpTlsMode, hash_mail_challenge_token,
            write_mail_settings,
        },
        config::Config,
        storage::{
            Database, MailChallengeKind, MailDeliveryErrorCode, MailOutboxStatus,
            MailOutboxTemplateKind, NewMailChallenge, NewMailOutboxMessage, NewStoredUser,
            StoredMailOutboxMessage,
        },
    };

    #[derive(Clone, Default)]
    struct RecordingSender {
        messages: Arc<Mutex<Vec<Vec<u8>>>>,
        should_fail: bool,
    }

    impl MailSender for RecordingSender {
        fn send<'a>(&'a self, message: Message) -> MailSendFuture<'a> {
            if let Ok(mut messages) = self.messages.lock() {
                messages.push(message.formatted());
            }
            let result = if self.should_fail {
                Err(MailSendFailure)
            } else {
                Ok(())
            };
            Box::pin(async move { result })
        }
    }

    async fn test_context() -> Result<(TempDir, Database, std::path::PathBuf), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let config_dir = directory.path().join("config");
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8100".parse()?,
            config_dir: config_dir.clone(),
        })
        .await?;
        write_mail_settings(
            &config_dir,
            Some(&MailSettings {
                host: "smtp.example.test".to_owned(),
                port: 465,
                tls_mode: SmtpTlsMode::Smtps,
                sender_name: "Lux".to_owned(),
                sender_email: "noreply@example.test".to_owned(),
                site_url: "https://lux.example.test/media".to_owned(),
            }),
        )
        .await?;
        Ok((directory, database, config_dir))
    }

    async fn enqueue_challenge(
        database: &Database,
        config_dir: &Path,
        kind: MailChallengeKind,
        now: i64,
        expires_at: i64,
    ) -> Result<(String, String), Box<dyn Error>> {
        let signer = MailChallengeSigner::load_or_create(config_dir).await?;
        let challenge_id = uuid::Uuid::now_v7().to_string();
        let token = signer
            .token_for(&challenge_id, kind, expires_at)
            .ok_or("test token could not be derived")?;
        let token_hash = hash_mail_challenge_token(&token);
        let (recipient_email, business_reference, template_kind) = match kind {
            MailChallengeKind::Invitation => (
                "invitee@example.test",
                "invitation-record-1",
                MailOutboxTemplateKind::Invitation,
            ),
            MailChallengeKind::EmailVerification => (
                "new-user@example.test",
                "registration-record-1",
                MailOutboxTemplateKind::EmailVerification,
            ),
        };
        let challenge = database
            .create_mail_challenge(
                NewMailChallenge {
                    id: &challenge_id,
                    kind,
                    recipient_email,
                    reference_id: business_reference,
                    token_hash: &token_hash,
                    expires_at,
                },
                now,
            )
            .await?;
        let idempotency_key = format!("mail-worker:{}", uuid::Uuid::now_v7());
        let outbox = database
            .enqueue_mail_outbox(
                NewMailOutboxMessage {
                    idempotency_key: &idempotency_key,
                    recipient_email,
                    template_kind,
                    reference_id: &challenge.id,
                    available_at: now,
                },
                now,
            )
            .await?;
        Ok((outbox.id, token))
    }

    async fn read_outbox(
        database: &Database,
        id: &str,
    ) -> Result<StoredMailOutboxMessage, Box<dyn Error>> {
        database
            .list_mail_outbox_page(None, 100, 0)
            .await?
            .messages
            .into_iter()
            .find(|message| message.id == id)
            .ok_or_else(|| "queued message not found".into())
    }

    #[tokio::test]
    async fn worker_rebuilds_invitation_link_and_marks_message_sent() -> Result<(), Box<dyn Error>>
    {
        let (_directory, database, config_dir) = test_context().await?;
        let now = 1_000;
        let (outbox_id, token) = enqueue_challenge(
            &database,
            &config_dir,
            MailChallengeKind::Invitation,
            now,
            now + 3_600,
        )
        .await?;
        let sender = RecordingSender::default();

        let outcome =
            process_next_mail(&database, &config_dir, now, |_, _| Ok(sender.clone())).await?;

        assert_eq!(outcome, MailWorkerStep::Sent);
        let messages = sender
            .messages
            .lock()
            .map_err(|_| "sender lock poisoned")?
            .clone();
        assert_eq!(messages.len(), 1);
        let message = String::from_utf8_lossy(&messages[0]);
        let encoded_body = message
            .split_once("\r\n\r\n")
            .map(|(_, body)| body.lines().collect::<String>())
            .ok_or("email body was missing")?;
        let body = String::from_utf8(STANDARD.decode(encoded_body.as_bytes())?)?;
        assert!(body.contains("https://lux.example.test/media/register/invitation#token="));
        assert!(body.contains(&token));
        assert!(!body.contains("?token="));
        let outbox = read_outbox(&database, &outbox_id).await?;
        assert_eq!(outbox.status, MailOutboxStatus::Sent);
        assert_eq!(outbox.attempt_count, 1);
        assert!(outbox.last_error_code.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn worker_renders_email_verification_link_for_its_challenge_type()
    -> Result<(), Box<dyn Error>> {
        let (_directory, database, config_dir) = test_context().await?;
        let now = 2_000;
        let (outbox_id, token) = enqueue_challenge(
            &database,
            &config_dir,
            MailChallengeKind::EmailVerification,
            now,
            now + 3_600,
        )
        .await?;
        let sender = RecordingSender::default();

        let outcome =
            process_next_mail(&database, &config_dir, now, |_, _| Ok(sender.clone())).await?;

        assert_eq!(outcome, MailWorkerStep::Sent);
        let messages = sender
            .messages
            .lock()
            .map_err(|_| "sender lock poisoned")?
            .clone();
        let message = String::from_utf8_lossy(&messages[0]);
        let encoded_body = message
            .split_once("\r\n\r\n")
            .map(|(_, body)| body.lines().collect::<String>())
            .ok_or("email body was missing")?;
        let body = String::from_utf8(STANDARD.decode(encoded_body.as_bytes())?)?;
        assert!(body.contains("https://lux.example.test/media/register/verify-email#token="));
        assert!(body.contains(&token));
        assert_eq!(
            read_outbox(&database, &outbox_id).await?.status,
            MailOutboxStatus::Sent
        );
        Ok(())
    }

    #[tokio::test]
    async fn worker_routes_existing_account_email_verification_to_account_page()
    -> Result<(), Box<dyn Error>> {
        let (_directory, database, config_dir) = test_context().await?;
        let now = 2_000;
        database
            .insert_user_with_email(NewStoredUser {
                id: "account-email-user",
                username_normalized: "account-email-user",
                display_name: "Account Email User",
                password_hash: "test-hash",
                is_admin: false,
                has_password: true,
                email: Some("old@example.test"),
            })
            .await?;
        let (outbox_id, token) = enqueue_account_email_change(
            &database,
            &config_dir,
            "account-email-user",
            now,
            now + 3_600,
        )
        .await?;
        let sender = RecordingSender::default();

        let outcome =
            process_next_mail(&database, &config_dir, now, |_, _| Ok(sender.clone())).await?;

        assert_eq!(outcome, MailWorkerStep::Sent);
        let messages = sender
            .messages
            .lock()
            .map_err(|_| "sender lock poisoned")?
            .clone();
        let message = String::from_utf8_lossy(&messages[0]);
        let encoded_body = message
            .split_once("\r\n\r\n")
            .map(|(_, body)| body.lines().collect::<String>())
            .ok_or("email body was missing")?;
        let body = String::from_utf8(STANDARD.decode(encoded_body.as_bytes())?)?;
        assert!(body.contains("https://lux.example.test/media/account/account-email-user#token="));
        assert!(body.contains(&token));
        assert_eq!(
            read_outbox(&database, &outbox_id).await?.status,
            MailOutboxStatus::Sent
        );
        Ok(())
    }

    async fn enqueue_account_email_change(
        database: &Database,
        config_dir: &Path,
        user_id: &str,
        now: i64,
        expires_at: i64,
    ) -> Result<(String, String), Box<dyn Error>> {
        let signer = MailChallengeSigner::load_or_create(config_dir).await?;
        let challenge_id = uuid::Uuid::now_v7().to_string();
        let token = signer
            .token_for(
                &challenge_id,
                MailChallengeKind::EmailVerification,
                expires_at,
            )
            .ok_or("test token could not be derived")?;
        let token_hash = hash_mail_challenge_token(&token);
        let challenge = database
            .create_mail_challenge(
                NewMailChallenge {
                    id: &challenge_id,
                    kind: MailChallengeKind::EmailVerification,
                    recipient_email: "new-address@example.test",
                    reference_id: user_id,
                    token_hash: &token_hash,
                    expires_at,
                },
                now,
            )
            .await?;
        let idempotency_key = format!("mail-worker:{}", uuid::Uuid::now_v7());
        let outbox = database
            .enqueue_mail_outbox(
                NewMailOutboxMessage {
                    idempotency_key: &idempotency_key,
                    recipient_email: "new-address@example.test",
                    template_kind: MailOutboxTemplateKind::EmailVerification,
                    reference_id: &challenge.id,
                    available_at: now,
                },
                now,
            )
            .await?;
        Ok((outbox.id, token))
    }

    #[tokio::test]
    async fn worker_fails_expired_challenges_without_sending() -> Result<(), Box<dyn Error>> {
        let (_directory, database, config_dir) = test_context().await?;
        let (outbox_id, _) = enqueue_challenge(
            &database,
            &config_dir,
            MailChallengeKind::EmailVerification,
            1_000,
            1_100,
        )
        .await?;
        let sender = RecordingSender::default();

        let outcome =
            process_next_mail(&database, &config_dir, 1_100, |_, _| Ok(sender.clone())).await?;

        assert_eq!(outcome, MailWorkerStep::Failed);
        assert!(
            sender
                .messages
                .lock()
                .map_err(|_| "sender lock poisoned")?
                .is_empty()
        );
        let outbox = read_outbox(&database, &outbox_id).await?;
        assert_eq!(outbox.status, MailOutboxStatus::Failed);
        assert_eq!(
            outbox.last_error_code,
            Some(MailDeliveryErrorCode::ReferenceMissing)
        );
        Ok(())
    }

    #[tokio::test]
    async fn smtp_failures_retry_then_stop_after_the_attempt_limit() -> Result<(), Box<dyn Error>> {
        let (_directory, database, config_dir) = test_context().await?;
        let (outbox_id, _) = enqueue_challenge(
            &database,
            &config_dir,
            MailChallengeKind::Invitation,
            1_000,
            10_000,
        )
        .await?;
        let sender = RecordingSender {
            should_fail: true,
            ..RecordingSender::default()
        };
        let mut now = 1_000;

        for attempt in 1..=MAX_MAIL_ATTEMPTS {
            let outcome =
                process_next_mail(&database, &config_dir, now, |_, _| Ok(sender.clone())).await?;
            let outbox = read_outbox(&database, &outbox_id).await?;
            assert_eq!(outbox.attempt_count, attempt);
            assert_eq!(
                outbox.last_error_code,
                Some(MailDeliveryErrorCode::SmtpUnavailable)
            );
            if attempt < MAX_MAIL_ATTEMPTS {
                assert_eq!(outcome, MailWorkerStep::Deferred);
                assert_eq!(outbox.status, MailOutboxStatus::RetryWait);
                now = outbox.available_at;
            } else {
                assert_eq!(outcome, MailWorkerStep::Failed);
                assert_eq!(outbox.status, MailOutboxStatus::Failed);
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn worker_shuts_down_without_waiting_for_the_poll_interval() -> Result<(), Box<dyn Error>>
    {
        let (_directory, database, config_dir) = test_context().await?;
        let worker = MailDeliveryWorker::start(database, config_dir);
        tokio::time::timeout(Duration::from_secs(1), worker.shutdown()).await?;
        Ok(())
    }
}
