use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ApiErrorCode {
    AuthenticationRequired,
    CsrfFailed,
    DatabaseUnavailable,
    Internal,
    InvalidRequest,
    LibraryPathUnavailable,
    LibraryPathNotWritable,
    LibraryRootDuplicate,
    LibraryRootOverlap,
    NotFound,
    PermissionDenied,
    InvalidCredentials,
    SetupAlreadyCompleted,
}

impl ApiErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AuthenticationRequired => "AUTHENTICATION_REQUIRED",
            Self::CsrfFailed => "CSRF_FAILED",
            Self::DatabaseUnavailable => "DATABASE_UNAVAILABLE",
            Self::Internal => "INTERNAL",
            Self::InvalidRequest => "INVALID_REQUEST",
            Self::LibraryPathUnavailable => "LIBRARY_PATH_UNAVAILABLE",
            Self::LibraryPathNotWritable => "LIBRARY_PATH_NOT_WRITABLE",
            Self::LibraryRootDuplicate => "LIBRARY_ROOT_DUPLICATE",
            Self::LibraryRootOverlap => "LIBRARY_ROOT_OVERLAP",
            Self::NotFound => "NOT_FOUND",
            Self::PermissionDenied => "PERMISSION_DENIED",
            Self::InvalidCredentials => "INVALID_CREDENTIALS",
            Self::SetupAlreadyCompleted => "SETUP_ALREADY_COMPLETED",
        }
    }
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
