use std::{
    fmt,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use image::ImageFormat;
use tokio::{fs, fs::OpenOptions, io::AsyncWriteExt};
use uuid::Uuid;

use crate::domain::ids::UserId;

const USER_AVATAR_DIRECTORY: &str = "user-avatars";
pub const MAX_USER_AVATAR_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Clone)]
pub struct UserAvatarService {
    directory: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredUserAvatar {
    pub bytes: Vec<u8>,
    pub content_type: &'static str,
}

impl UserAvatarService {
    pub fn new(config_dir: PathBuf) -> Self {
        Self {
            directory: config_dir.join(USER_AVATAR_DIRECTORY),
        }
    }

    pub async fn store(
        &self,
        user_id: UserId,
        content_type: &str,
        bytes: &[u8],
    ) -> Result<(), UserAvatarError> {
        let size = u64::try_from(bytes.len()).map_err(|_| UserAvatarError::TooLarge {
            size: u64::MAX,
            max: MAX_USER_AVATAR_BYTES,
        })?;
        if size > MAX_USER_AVATAR_BYTES {
            return Err(UserAvatarError::TooLarge {
                size,
                max: MAX_USER_AVATAR_BYTES,
            });
        }
        avatar_content_type(content_type, bytes)?;
        create_private_dir(&self.directory).await?;
        write_atomically(&self.avatar_path(user_id), bytes).await
    }

    pub async fn load(&self, user_id: UserId) -> Result<Option<StoredUserAvatar>, UserAvatarError> {
        let path = self.avatar_path(user_id);
        let metadata = match fs::symlink_metadata(&path).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(UserAvatarError::io(path, source)),
        };
        if metadata.file_type().is_symlink() {
            return Err(UserAvatarError::InvalidPath(path));
        }
        let bytes = fs::read(&path)
            .await
            .map_err(|source| UserAvatarError::io(path.clone(), source))?;
        let content_type = detected_content_type(&bytes).ok_or(UserAvatarError::InvalidContent)?;
        Ok(Some(StoredUserAvatar {
            bytes,
            content_type,
        }))
    }

    fn avatar_path(&self, user_id: UserId) -> PathBuf {
        self.directory.join(user_id.to_string())
    }
}

fn avatar_content_type(content_type: &str, bytes: &[u8]) -> Result<&'static str, UserAvatarError> {
    let requested = content_type
        .split(';')
        .next()
        .map(str::trim)
        .unwrap_or_default();
    let detected = detected_content_type(bytes).ok_or(UserAvatarError::InvalidContent)?;
    if !matches!(requested, "image/jpeg" | "image/png" | "image/webp") {
        return Err(UserAvatarError::UnsupportedContentType);
    }
    if requested != detected {
        return Err(UserAvatarError::InvalidContent);
    }
    Ok(detected)
}

fn detected_content_type(bytes: &[u8]) -> Option<&'static str> {
    match image::guess_format(bytes).ok()? {
        ImageFormat::Jpeg => Some("image/jpeg"),
        ImageFormat::Png => Some("image/png"),
        ImageFormat::WebP => Some("image/webp"),
        _ => None,
    }
}

async fn create_private_dir(path: &Path) -> Result<(), UserAvatarError> {
    fs::create_dir_all(path)
        .await
        .map_err(|source| UserAvatarError::io(path.to_owned(), source))?;
    #[cfg(unix)]
    fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .await
        .map_err(|source| UserAvatarError::io(path.to_owned(), source))?;
    Ok(())
}

async fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), UserAvatarError> {
    let parent = path
        .parent()
        .ok_or_else(|| UserAvatarError::InvalidPath(path.to_owned()))?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| UserAvatarError::InvalidPath(path.to_owned()))?;
    let temporary = parent.join(format!(".{file_name}.{}.tmp", Uuid::now_v7()));
    let result = async {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .await
            .map_err(|source| UserAvatarError::io(temporary.clone(), source))?;
        file.write_all(bytes)
            .await
            .map_err(|source| UserAvatarError::io(temporary.clone(), source))?;
        file.sync_all()
            .await
            .map_err(|source| UserAvatarError::io(temporary.clone(), source))?;
        drop(file);
        #[cfg(unix)]
        fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))
            .await
            .map_err(|source| UserAvatarError::io(temporary.clone(), source))?;
        fs::rename(&temporary, path)
            .await
            .map_err(|source| UserAvatarError::io(path.to_owned(), source))?;
        let directory = fs::File::open(parent)
            .await
            .map_err(|source| UserAvatarError::io(parent.to_owned(), source))?;
        directory
            .sync_all()
            .await
            .map_err(|source| UserAvatarError::io(parent.to_owned(), source))?;
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_file(&temporary).await;
    }
    result
}

#[derive(Debug)]
pub enum UserAvatarError {
    UnsupportedContentType,
    InvalidContent,
    TooLarge {
        size: u64,
        max: u64,
    },
    InvalidPath(PathBuf),
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl UserAvatarError {
    fn io(path: PathBuf, source: std::io::Error) -> Self {
        Self::Io { path, source }
    }
}

impl fmt::Display for UserAvatarError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedContentType => {
                formatter.write_str("avatar content type is unsupported")
            }
            Self::InvalidContent => formatter.write_str("avatar content is invalid"),
            Self::TooLarge { size, max } => {
                write!(formatter, "avatar is too large: {size} > {max}")
            }
            Self::InvalidPath(path) => {
                write!(formatter, "avatar path is invalid: {}", path.display())
            }
            Self::Io { path, source } => write!(
                formatter,
                "avatar I/O failed for {}: {source}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for UserAvatarError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::UnsupportedContentType
            | Self::InvalidContent
            | Self::TooLarge { .. }
            | Self::InvalidPath(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use image::{DynamicImage, ImageFormat};
    use tempfile::tempdir;

    use super::{MAX_USER_AVATAR_BYTES, UserAvatarError, UserAvatarService};
    use crate::domain::ids::UserId;

    fn png_bytes() -> Vec<u8> {
        let mut output = Cursor::new(Vec::new());
        DynamicImage::new_rgba8(1, 1)
            .write_to(&mut output, ImageFormat::Png)
            .expect("png fixture should encode");
        output.into_inner()
    }

    #[tokio::test]
    async fn stores_and_loads_a_valid_png_for_one_user() {
        let directory = tempdir().expect("temporary directory should be available");
        let service = UserAvatarService::new(directory.path().to_owned());
        let user_id = UserId::new();
        let bytes = png_bytes();

        service
            .store(user_id, "image/png", &bytes)
            .await
            .expect("valid avatar should be stored");

        let avatar = service
            .load(user_id)
            .await
            .expect("avatar should load")
            .expect("stored avatar should exist");
        assert_eq!(avatar.content_type, "image/png");
        assert_eq!(avatar.bytes, bytes);
    }

    #[tokio::test]
    async fn rejects_mismatched_and_oversized_uploads() {
        let directory = tempdir().expect("temporary directory should be available");
        let service = UserAvatarService::new(directory.path().to_owned());
        let user_id = UserId::new();
        let bytes = png_bytes();

        assert!(matches!(
            service.store(user_id, "image/jpeg", &bytes).await,
            Err(UserAvatarError::InvalidContent)
        ));
        assert!(matches!(
            service
                .store(
                    user_id,
                    "image/png",
                    &vec![0; MAX_USER_AVATAR_BYTES as usize + 1]
                )
                .await,
            Err(UserAvatarError::TooLarge { .. })
        ));
    }
}
