use std::{
    fmt,
    path::{Path, PathBuf},
};

use reqwest::{Client, Url, header::CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use tokio::{fs, io::AsyncWriteExt};
use uuid::Uuid;

const PEOPLE_DIR: &str = "people";
const ITEMS_DIR: &str = "items";
const PROFILES_DIR: &str = "profiles";
const MAX_ACTORS: usize = 12;
const MAX_PEOPLE_FILE_BYTES: u64 = 256 * 1024;
const MAX_PROFILE_BYTES: usize = 10 * 1024 * 1024;
const PROFILE_EXTENSIONS: [&str; 3] = ["jpg", "png", "webp"];

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActorCredit {
    pub id: i64,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoredActor {
    id: i64,
    name: String,
    character: Option<String>,
    order: Option<i32>,
    image_file: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActorView {
    pub id: String,
    pub name: String,
    pub character: Option<String>,
    pub image_url: Option<String>,
}

#[derive(Clone, Debug)]
pub struct PersonImage {
    pub path: PathBuf,
    pub content_type: &'static str,
    pub content_length: u64,
}

#[derive(Clone)]
pub struct PeopleService {
    config_dir: PathBuf,
    client: Client,
}

impl PeopleService {
    pub fn new(config_dir: PathBuf) -> Self {
        Self {
            config_dir,
            client: Client::new(),
        }
    }

    pub async fn persist_item_actors(
        &self,
        item_id: &str,
        actors: &[ActorCredit],
    ) -> Result<usize, PeopleError> {
        validate_component(item_id)?;
        let items_dir = self.people_dir().join(ITEMS_DIR);
        let profiles_dir = self.people_dir().join(PROFILES_DIR);
        create_private_dir(&items_dir).await?;
        create_private_dir(&profiles_dir).await?;

        let mut stored = Vec::new();
        for actor in actors.iter().take(MAX_ACTORS) {
            if actor.id <= 0 || actor.name.trim().is_empty() {
                continue;
            }
            let image_file = if let Some(url) = actor.profile_url.as_deref() {
                match self
                    .ensure_profile_image(actor.id, url, &profiles_dir)
                    .await
                {
                    Ok(image_file) => image_file,
                    Err(error) => {
                        tracing::warn!(
                            person_id = actor.id,
                            %error,
                            "actor profile image was not cached"
                        );
                        None
                    }
                }
            } else {
                None
            };
            stored.push(StoredActor {
                id: actor.id,
                name: actor.name.trim().to_owned(),
                character: actor
                    .character
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned),
                order: actor.order,
                image_file,
            });
        }

        stored.sort_by_key(|actor| match actor.order {
            Some(order) => order,
            None => i32::MAX,
        });
        let bytes = serde_json::to_vec_pretty(&stored)
            .map_err(|source| PeopleError::Serialization(source.to_string()))?;
        let path = items_dir.join(format!("{item_id}.json"));
        write_atomically(&path, &bytes).await?;
        Ok(stored.len())
    }

    pub async fn list_item_actors(&self, item_id: &str) -> Result<Vec<ActorView>, PeopleError> {
        validate_component(item_id)?;
        let path = self
            .people_dir()
            .join(ITEMS_DIR)
            .join(format!("{item_id}.json"));
        if let Some(metadata) = safe_metadata(&path).await?
            && !metadata.is_file()
        {
            return Err(PeopleError::Serialization(
                "people data path is not a file".to_owned(),
            ));
        }
        let bytes = match fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(PeopleError::Io { path, source }),
        };
        if bytes.len() as u64 > MAX_PEOPLE_FILE_BYTES {
            return Err(PeopleError::Serialization(
                "people data is too large".to_owned(),
            ));
        }
        let actors = serde_json::from_slice::<Vec<StoredActor>>(&bytes)
            .map_err(|source| PeopleError::Serialization(source.to_string()))?;
        let mut views = Vec::new();
        for actor in actors
            .into_iter()
            .take(MAX_ACTORS)
            .filter(|actor| actor.id > 0 && !actor.name.trim().is_empty())
        {
            let id = actor.id.to_string();
            let image_url =
                if actor.image_file.is_some() && self.profile_image(&id).await?.is_some() {
                    Some(format!("/api/v1/people/{id}/image"))
                } else {
                    None
                };
            views.push(ActorView {
                id,
                name: actor.name,
                character: actor.character,
                image_url,
            });
        }
        Ok(views)
    }

    pub async fn profile_image(&self, person_id: &str) -> Result<Option<PersonImage>, PeopleError> {
        let person_id = person_id
            .parse::<i64>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| PeopleError::InvalidComponent(person_id.to_owned()))?;
        let profiles_dir = self.people_dir().join(PROFILES_DIR);
        for extension in PROFILE_EXTENSIONS {
            let path = profiles_dir.join(format!("{person_id}.{extension}"));
            let Some(metadata) = safe_metadata(&path).await? else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            let content_type = match extension {
                "jpg" => "image/jpeg",
                "png" => "image/png",
                "webp" => "image/webp",
                _ => continue,
            };
            return Ok(Some(PersonImage {
                path,
                content_type,
                content_length: metadata.len(),
            }));
        }
        Ok(None)
    }

    fn people_dir(&self) -> PathBuf {
        self.config_dir.join(PEOPLE_DIR)
    }

    async fn ensure_profile_image(
        &self,
        person_id: i64,
        image_url: &str,
        profiles_dir: &Path,
    ) -> Result<Option<String>, PeopleError> {
        for extension in PROFILE_EXTENSIONS {
            let path = profiles_dir.join(format!("{person_id}.{extension}"));
            if safe_metadata(&path)
                .await?
                .is_some_and(|metadata| metadata.is_file())
            {
                return Ok(Some(format!("{PROFILES_DIR}/{person_id}.{extension}")));
            }
        }

        let url =
            Url::parse(image_url).map_err(|source| PeopleError::InvalidUrl(source.to_string()))?;
        if url.scheme() != "https"
            || url.host_str() != Some("image.tmdb.org")
            || !url.path().starts_with("/t/p/")
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(PeopleError::InvalidUrl(
                "actor profile URL must be an HTTPS TMDb image path".to_owned(),
            ));
        }
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|source| PeopleError::Download(source.to_string()))?;
        if !response.status().is_success() {
            return Err(PeopleError::UpstreamStatus(response.status().as_u16()));
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::trim)
            .ok_or_else(|| {
                PeopleError::InvalidImage("profile image content type is missing".to_owned())
            })?;
        let (extension, expected_type) = match content_type {
            "image/jpeg" => ("jpg", "image/jpeg"),
            "image/png" => ("png", "image/png"),
            "image/webp" => ("webp", "image/webp"),
            other => {
                return Err(PeopleError::InvalidImage(format!(
                    "unsupported profile image type: {other}"
                )));
            }
        };
        let bytes = response
            .bytes()
            .await
            .map_err(|source| PeopleError::Download(source.to_string()))?;
        if bytes.is_empty()
            || bytes.len() > MAX_PROFILE_BYTES
            || !valid_image(expected_type, &bytes)
        {
            return Err(PeopleError::InvalidImage(
                "profile image payload is invalid".to_owned(),
            ));
        }
        let path = profiles_dir.join(format!("{person_id}.{extension}"));
        write_atomically(&path, &bytes).await?;
        Ok(Some(format!("{PROFILES_DIR}/{person_id}.{extension}")))
    }
}

fn validate_component(value: &str) -> Result<(), PeopleError> {
    if value.is_empty()
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(PeopleError::InvalidComponent(value.to_owned()));
    }
    Ok(())
}

async fn safe_metadata(path: &Path) -> Result<Option<std::fs::Metadata>, PeopleError> {
    match fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(PeopleError::Symlink(path.to_owned()))
        }
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(PeopleError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

async fn create_private_dir(path: &Path) -> Result<(), PeopleError> {
    fs::create_dir_all(path)
        .await
        .map_err(|source| PeopleError::Io {
            path: path.to_owned(),
            source,
        })?;
    restrict_permissions(path, true).await
}

async fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), PeopleError> {
    let parent = path.parent().ok_or_else(|| PeopleError::Io {
        path: path.to_owned(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing parent"),
    })?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| PeopleError::Io {
            path: path.to_owned(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid file name"),
        })?;
    let temporary = parent.join(format!(".{file_name}.{}.tmp", Uuid::now_v7()));
    let result = async {
        let mut file = fs::File::create(&temporary)
            .await
            .map_err(|source| PeopleError::Io {
                path: temporary.clone(),
                source,
            })?;
        file.write_all(bytes)
            .await
            .map_err(|source| PeopleError::Io {
                path: temporary.clone(),
                source,
            })?;
        file.sync_all().await.map_err(|source| PeopleError::Io {
            path: temporary.clone(),
            source,
        })?;
        drop(file);
        fs::rename(&temporary, path)
            .await
            .map_err(|source| PeopleError::Io {
                path: path.to_owned(),
                source,
            })?;
        restrict_permissions(path, false).await
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_file(&temporary).await;
    }
    result
}

async fn restrict_permissions(path: &Path, directory: bool) -> Result<(), PeopleError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if directory { 0o700 } else { 0o600 };
        fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .await
            .map_err(|source| PeopleError::Io {
                path: path.to_owned(),
                source,
            })?;
    }
    #[cfg(not(unix))]
    let _ = (path, directory);
    Ok(())
}

fn valid_image(content_type: &str, bytes: &[u8]) -> bool {
    match content_type {
        "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
        _ => false,
    }
}

#[derive(Debug)]
pub enum PeopleError {
    InvalidComponent(String),
    InvalidUrl(String),
    InvalidImage(String),
    UpstreamStatus(u16),
    Download(String),
    Serialization(String),
    Symlink(PathBuf),
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl fmt::Display for PeopleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidComponent(_) => formatter.write_str("invalid people path component"),
            Self::InvalidUrl(message) | Self::InvalidImage(message) | Self::Download(message) => {
                formatter.write_str(message)
            }
            Self::UpstreamStatus(status) => {
                write!(formatter, "people image upstream returned {status}")
            }
            Self::Serialization(message) => write!(formatter, "people data is invalid: {message}"),
            Self::Symlink(path) => {
                write!(formatter, "people path is a symlink: {}", path.display())
            }
            Self::Io { path, source } => {
                write!(formatter, "people file {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for PeopleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::InvalidComponent(_)
            | Self::InvalidUrl(_)
            | Self::InvalidImage(_)
            | Self::UpstreamStatus(_)
            | Self::Download(_)
            | Self::Serialization(_)
            | Self::Symlink(_) => None,
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::{PeopleError, PeopleService};

    #[tokio::test]
    async fn profile_image_rejects_symlinked_files() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let config = tempfile::tempdir()?;
        let profiles = config.path().join("people/profiles");
        tokio::fs::create_dir_all(&profiles).await?;
        let outside = config.path().join("outside.png");
        tokio::fs::write(&outside, b"not an image").await?;
        symlink(&outside, profiles.join("9.png"))?;

        let error = PeopleService::new(config.path().to_owned())
            .profile_image("9")
            .await
            .expect_err("symlinked profile must be rejected");
        assert!(matches!(error, PeopleError::Symlink(_)));
        Ok(())
    }
}
