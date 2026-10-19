use serde::Serialize;

use crate::{
    auth::users::{UserStore, UserStoreError, UserUpdate},
    domain::ids::UserId,
};

pub(crate) const MAX_BATCH_USER_STATUS_TARGETS: usize = 100;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum BatchUserStatusErrorCode {
    InvalidUserId,
    UserNotFound,
    LastManager,
    StorageUnavailable,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BatchUserStatusItemResult {
    pub(crate) user_id: String,
    pub(crate) success: bool,
    pub(crate) error_code: Option<BatchUserStatusErrorCode>,
    #[serde(skip)]
    pub(crate) hls_session_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BatchUserStatusResult {
    pub(crate) is_disabled: bool,
    pub(crate) results: Vec<BatchUserStatusItemResult>,
}

pub(crate) async fn update_users_disabled_status(
    users: &UserStore,
    user_ids: &[String],
    is_disabled: bool,
) -> BatchUserStatusResult {
    let mut results = Vec::with_capacity(user_ids.len());
    for user_id in user_ids {
        let Ok(user_id) = user_id.parse::<UserId>() else {
            results.push(BatchUserStatusItemResult {
                user_id: user_id.clone(),
                success: false,
                error_code: Some(BatchUserStatusErrorCode::InvalidUserId),
                hls_session_ids: Vec::new(),
            });
            continue;
        };
        let user_id = user_id.to_string();
        let outcome = match users
            .update_user_with_hls_session_ids(
                &user_id,
                UserUpdate {
                    is_disabled: Some(is_disabled),
                    ..UserUpdate::default()
                },
            )
            .await
        {
            Ok(Some((_, hls_session_ids))) => BatchUserStatusItemResult {
                user_id,
                success: true,
                error_code: None,
                hls_session_ids,
            },
            Ok(None) => BatchUserStatusItemResult {
                user_id,
                success: false,
                error_code: Some(BatchUserStatusErrorCode::UserNotFound),
                hls_session_ids: Vec::new(),
            },
            Err(UserStoreError::LastManager) => BatchUserStatusItemResult {
                user_id,
                success: false,
                error_code: Some(BatchUserStatusErrorCode::LastManager),
                hls_session_ids: Vec::new(),
            },
            Err(_) => BatchUserStatusItemResult {
                user_id,
                success: false,
                error_code: Some(BatchUserStatusErrorCode::StorageUnavailable),
                hls_session_ids: Vec::new(),
            },
        };
        results.push(outcome);
    }
    BatchUserStatusResult {
        is_disabled,
        results,
    }
}
