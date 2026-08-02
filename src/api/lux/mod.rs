use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ApiErrorCode {
    DatabaseUnavailable,
    Internal,
    InvalidRequest,
    LibraryPathNotWritable,
    NotFound,
    SetupAlreadyCompleted,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    pub code: ApiErrorCode,
    pub message: String,
    pub request_id: String,
}

impl ApiError {
    pub fn new(
        code: ApiErrorCode,
        message: impl Into<String>,
        request_id: impl Into<String>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            request_id: request_id.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ApiErrorEnvelope {
    pub error: ApiError,
}

impl From<ApiError> for ApiErrorEnvelope {
    fn from(error: ApiError) -> Self {
        Self { error }
    }
}
