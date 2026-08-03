use std::{
    fmt,
    path::{Component, Path, PathBuf},
};

use sha2::{Digest, Sha256};
use tokio::fs;

use crate::{
    application::images::{ImageWriteError, write_image_atomically},
    domain::ids::LibraryId,
    storage::{Database, StorageError},
};

pub const MAX_LIBRARY_COVER_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Clone)]
pub struct LibraryCoverService {
    database: Database,
    directory: PathBuf,
}

impl LibraryCoverService {
    pub fn new(database: Database, directory: PathBuf) -> Self {
        Self {
            database,
            directory,
        }
    }

    pub async fn store(
        &self,
        library_id: LibraryId,
        content_type: &str,
        bytes: &[u8],
    ) -> Result<StoredLibraryCover, LibraryCoverError> {
        let format = ImageFormat::from_content_type(content_type)
            .ok_or_else(|| LibraryCoverError::UnsupportedContentType(content_type.to_owned()))?;
        validate_payload(format, bytes)?;
        let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if size > MAX_LIBRARY_COVER_BYTES {
            return Err(LibraryCoverError::TooLarge {
                size,
                max: MAX_LIBRARY_COVER_BYTES,
            });
        }

        let library_id_text = library_id.to_string();
        let library = self
            .database
            .find_library(&library_id_text)
            .await?
            .ok_or(LibraryCoverError::LibraryNotFound)?;
        fs::create_dir_all(&self.directory)
            .await
            .map_err(|source| image_io_error(&self.directory, source))?;

        let file_name = format!("{library_id_text}.{}", format.extension());
        let target = self.directory.join(&file_name);
        write_image_atomically(&target, bytes).await?;
        let tag = content_tag(bytes);
        let size = i64::try_from(size).map_err(|_| LibraryCoverError::TooLarge {
            size,
            max: i64::MAX as u64,
        })?;
        if !self
            .database
            .update_library_cover(
                &library_id_text,
                &file_name,
                format.content_type(),
                size,
                &tag,
            )
            .await?
        {
            let _ = fs::remove_file(&target).await;
            return Err(LibraryCoverError::LibraryNotFound);
        }

        if let Some(previous) = library.cover_image_path.as_deref()
            && previous != file_name
        {
            let previous_path = self.cover_path(previous)?;
            match fs::remove_file(previous_path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => return Err(image_io_error(&self.directory, source)),
            }
        }

        Ok(StoredLibraryCover {
            path: target,
            content_type: format.content_type().to_owned(),
            content_length: u64::try_from(size).unwrap_or_default(),
            etag: format!("\"{tag}\""),
        })
    }

    pub async fn resolve(
        &self,
        library_id: LibraryId,
    ) -> Result<Option<StoredLibraryCover>, LibraryCoverError> {
        let library_id_text = library_id.to_string();
        let Some(library) = self.database.find_library(&library_id_text).await? else {
            return Ok(None);
        };
        let Some(relative_path) = library.cover_image_path.as_deref() else {
            return Ok(None);
        };
        let path = self.cover_path(relative_path)?;
        let metadata = match fs::symlink_metadata(&path).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(image_io_error(&path, source)),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Ok(None);
        }
        let Some(content_type) = library.cover_image_content_type else {
            return Ok(None);
        };
        let Some(tag) = library.cover_image_tag else {
            return Ok(None);
        };
        Ok(Some(StoredLibraryCover {
            path,
            content_type,
            content_length: metadata.len(),
            etag: format!("\"{tag}\""),
        }))
    }

    fn cover_path(&self, relative_path: &str) -> Result<PathBuf, LibraryCoverError> {
        let mut components = Path::new(relative_path).components();
        let Some(Component::Normal(file_name)) = components.next() else {
            return Err(LibraryCoverError::InvalidPath);
        };
        if components.next().is_some() {
            return Err(LibraryCoverError::InvalidPath);
        }
        Ok(self.directory.join(file_name))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredLibraryCover {
    pub path: PathBuf,
    pub content_type: String,
    pub content_length: u64,
    pub etag: String,
}

#[derive(Debug)]
pub enum LibraryCoverError {
    UnsupportedContentType(String),
    InvalidContent {
        content_type: &'static str,
    },
    TooLarge {
        size: u64,
        max: u64,
    },
    InvalidPath,
    LibraryNotFound,
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    ImageWrite(ImageWriteError),
    Storage(StorageError),
}

impl fmt::Display for LibraryCoverError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedContentType(content_type) => {
                write!(
                    formatter,
                    "unsupported library cover content type: {content_type}"
                )
            }
            Self::InvalidContent { content_type } => {
                write!(formatter, "invalid image content for {content_type}")
            }
            Self::TooLarge { size, max } => {
                write!(
                    formatter,
                    "library cover is too large: {size} bytes, maximum {max}"
                )
            }
            Self::InvalidPath => formatter.write_str("library cover path is invalid"),
            Self::LibraryNotFound => formatter.write_str("library not found"),
            Self::Io { path, source } => {
                write!(formatter, "library cover '{}': {source}", path.display())
            }
            Self::ImageWrite(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for LibraryCoverError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::ImageWrite(error) => Some(error),
            Self::Storage(error) => Some(error),
            Self::UnsupportedContentType(_)
            | Self::InvalidContent { .. }
            | Self::TooLarge { .. }
            | Self::InvalidPath
            | Self::LibraryNotFound => None,
        }
    }
}

impl From<ImageWriteError> for LibraryCoverError {
    fn from(error: ImageWriteError) -> Self {
        Self::ImageWrite(error)
    }
}

impl From<StorageError> for LibraryCoverError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

#[derive(Clone, Copy)]
enum ImageFormat {
    Jpeg,
    Png,
    Webp,
}

impl ImageFormat {
    fn from_content_type(value: &str) -> Option<Self> {
        match value
            .split(';')
            .next()?
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "image/jpeg" | "image/jpg" => Some(Self::Jpeg),
            "image/png" => Some(Self::Png),
            "image/webp" => Some(Self::Webp),
            _ => None,
        }
    }

    const fn content_type(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Webp => "image/webp",
        }
    }

    const fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::Webp => "webp",
        }
    }
}

fn validate_payload(format: ImageFormat, bytes: &[u8]) -> Result<(), LibraryCoverError> {
    let valid = match format {
        ImageFormat::Jpeg => {
            bytes.len() >= 4
                && bytes.starts_with(&[0xff, 0xd8, 0xff])
                && bytes.ends_with(&[0xff, 0xd9])
        }
        ImageFormat::Png => {
            bytes.len() >= 24
                && bytes.starts_with(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])
                && bytes.windows(4).any(|chunk| chunk == b"IEND")
        }
        ImageFormat::Webp => {
            bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP"
        }
    };
    if valid {
        Ok(())
    } else {
        Err(LibraryCoverError::InvalidContent {
            content_type: format.content_type(),
        })
    }
}

fn content_tag(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn image_io_error(path: &Path, source: std::io::Error) -> LibraryCoverError {
    LibraryCoverError::Io {
        path: path.to_owned(),
        source,
    }
}
