use std::{
    fmt,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

const MAX_COMPONENT_CHARS: usize = 96;

pub const METADATA_DIR: &str = "metadata";
pub const LIBRARY_DIR: &str = "library";
pub const PEOPLE_DIR: &str = "people";
pub const PEOPLE_INDEX_DIR: &str = "index";
pub const MOVIE_NFO_METADATA_FILE: &str = "movie.nfo.json";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetadataObjectKind {
    Collection,
    Genre,
    Studio,
    Tag,
}

impl MetadataObjectKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Collection => "collections",
            Self::Genre => "genres",
            Self::Studio => "studios",
            Self::Tag => "tags",
        }
    }

    pub(crate) const fn file_name(self) -> &'static str {
        match self {
            Self::Collection => "collection.json",
            Self::Genre => "genre.json",
            Self::Studio => "studio.json",
            Self::Tag => "tag.json",
        }
    }
}

pub fn metadata_root(config_dir: &Path) -> PathBuf {
    config_dir.join(METADATA_DIR)
}

pub fn library_item_directory(
    config_dir: &Path,
    item_id: &str,
) -> Result<PathBuf, MetadataPathError> {
    validate_component(item_id, "item ID")?;
    Ok(metadata_root(config_dir)
        .join(LIBRARY_DIR)
        .join(stable_shard(item_id))
        .join(item_id))
}

pub fn library_item_nfo_path(
    config_dir: &Path,
    item_id: &str,
) -> Result<PathBuf, MetadataPathError> {
    Ok(library_item_directory(config_dir, item_id)?.join(MOVIE_NFO_METADATA_FILE))
}

pub fn people_directory(
    config_dir: &Path,
    display_name: &str,
    provider: &str,
    provider_id: &str,
) -> Result<PathBuf, MetadataPathError> {
    validate_component(provider, "provider")?;
    validate_component(provider_id, "provider ID")?;
    let display_name = display_name.trim();
    if display_name.is_empty() {
        return Err(MetadataPathError::EmptyComponent("display name"));
    }
    let bucket = display_name
        .chars()
        .find(|character| character.is_alphanumeric())
        .map(|character| character.to_string())
        .unwrap_or_else(|| "_".to_owned());
    let display_name = readable_component(display_name);
    let provider = ascii_component(provider, "provider")?;
    Ok(metadata_root(config_dir)
        .join(PEOPLE_DIR)
        .join(bucket)
        .join(format!("{display_name}-{provider}-{provider_id}")))
}

pub fn people_index_path(config_dir: &Path, person_id: &str) -> Result<PathBuf, MetadataPathError> {
    validate_component(person_id, "person ID")?;
    Ok(metadata_root(config_dir)
        .join(PEOPLE_DIR)
        .join(PEOPLE_INDEX_DIR)
        .join(format!("{person_id}.json")))
}

pub fn people_index_directory(config_dir: &Path) -> PathBuf {
    metadata_root(config_dir)
        .join(PEOPLE_DIR)
        .join(PEOPLE_INDEX_DIR)
}

pub fn metadata_object_directory(
    config_dir: &Path,
    kind: MetadataObjectKind,
    display_name: &str,
    provider: &str,
    object_id: &str,
) -> Result<PathBuf, MetadataPathError> {
    validate_component(provider, "provider")?;
    validate_component(object_id, "object ID")?;
    let display_name = display_name.trim();
    if display_name.is_empty() {
        return Err(MetadataPathError::EmptyComponent("display name"));
    }
    let bucket = display_name
        .chars()
        .find(|character| character.is_alphanumeric())
        .map(|character| character.to_string())
        .unwrap_or_else(|| "_".to_owned());
    let display_name = readable_component(display_name);
    let provider = ascii_component(provider, "provider")?;
    Ok(metadata_root(config_dir)
        .join(kind.as_str())
        .join(bucket)
        .join(format!("{display_name}-{provider}-{object_id}")))
}

fn stable_shard(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    format!("{:02x}", digest[0])
}

fn validate_component(value: &str, label: &'static str) -> Result<(), MetadataPathError> {
    if value.is_empty() {
        return Err(MetadataPathError::EmptyComponent(label));
    }
    if value.len() > 128
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(MetadataPathError::InvalidComponent(label));
    }
    Ok(())
}

fn ascii_component(value: &str, label: &'static str) -> Result<String, MetadataPathError> {
    if value.is_empty() {
        Err(MetadataPathError::EmptyComponent(label))
    } else {
        Ok(value.to_ascii_lowercase())
    }
}

fn readable_component(value: &str) -> String {
    let mut result = String::new();
    for character in value.chars() {
        let character = if character.is_alphanumeric()
            || matches!(character, '-' | '_' | '.' | '·' | '(' | ')' | '[' | ']')
        {
            character
        } else if character.is_whitespace() {
            '-'
        } else {
            '_'
        };
        if result.chars().count() < MAX_COMPONENT_CHARS {
            result.push(character);
        }
    }
    let result = result.trim_matches(['-', '_', '.']).to_owned();
    if result.is_empty() {
        "unknown".to_owned()
    } else {
        result
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetadataPathError {
    EmptyComponent(&'static str),
    InvalidComponent(&'static str),
}

impl fmt::Display for MetadataPathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyComponent(label) => write!(formatter, "{label} must not be empty"),
            Self::InvalidComponent(label) => {
                write!(formatter, "{label} is not a safe path component")
            }
        }
    }
}

impl std::error::Error for MetadataPathError {}

#[cfg(test)]
mod tests {
    use super::readable_component;

    #[test]
    fn readable_component_preserves_cjk_and_common_name_punctuation() {
        assert_eq!(readable_component("阿·米切尔 / 试验"), "阿·米切尔-_-试验");
    }

    #[test]
    fn readable_component_has_a_non_empty_fallback() {
        assert_eq!(readable_component("///"), "unknown");
    }
}
