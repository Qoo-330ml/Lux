use super::*;

impl PeopleService {
    pub async fn profile_image(&self, person_id: &str) -> Result<Option<PersonImage>, PeopleError> {
        self.profile_image_for_provider(None, person_id).await
    }

    pub async fn update_person_image(
        &self,
        person_id: &str,
        person_name: &str,
        provider: Option<&str>,
        content_type: Option<&str>,
        bytes: &[u8],
    ) -> Result<(), PeopleError> {
        validate_component(person_id)?;
        if !is_valid_person_lookup(person_name) {
            return Err(PeopleError::InvalidComponent(person_name.to_owned()));
        }
        if bytes.is_empty() || bytes.len() > MAX_PROFILE_BYTES {
            return Err(PeopleError::InvalidImage(
                "profile image payload is invalid".to_owned(),
            ));
        }
        let image_bytes = if profile_image_format(content_type, bytes).is_some() {
            bytes.to_owned()
        } else {
            BASE64.decode(bytes).map_err(|_| {
                PeopleError::InvalidImage("unsupported profile image type".to_owned())
            })?
        };
        let (extension, expected_type) = profile_image_format(content_type, &image_bytes)
            .ok_or_else(|| {
                PeopleError::InvalidImage("unsupported profile image type".to_owned())
            })?;
        if image_bytes.len() > MAX_PROFILE_BYTES || !valid_image(expected_type, &image_bytes) {
            return Err(PeopleError::InvalidImage(
                "profile image payload is invalid".to_owned(),
            ));
        }

        let provider = provider.unwrap_or("tmdb").trim().to_ascii_lowercase();
        validate_component(&provider)?;
        let person_dir = self
            .person_directory_for_update(person_name, &provider, person_id)
            .await?;
        create_private_dir(&person_dir).await?;
        let image_path = format!("{PERSON_IMAGE}.{extension}");
        write_atomically(&person_dir.join(&image_path), &image_bytes).await?;
        let relative_image_path = person_dir
            .join(&image_path)
            .strip_prefix(metadata_root(&self.config_dir))
            .map_err(|_| {
                PeopleError::Serialization("person image path is outside metadata".to_owned())
            })?
            .to_string_lossy()
            .into_owned();
        create_private_dir(&people_index_directory(&self.config_dir)).await?;
        let index = StoredPersonIndex {
            image_path: relative_image_path,
            person_key: person_key_for_identities(&[PersonIdentity {
                provider: provider.clone(),
                id: person_id.to_owned(),
            }]),
        };
        let index_bytes = serde_json::to_vec_pretty(&index)
            .map_err(|source| PeopleError::Serialization(source.to_string()))?;
        let provider_index_path =
            people_index_path_for_provider(&self.config_dir, &provider, person_id)
                .map_err(PeopleError::from)?;
        write_atomically(&provider_index_path, &index_bytes).await?;
        if provider.eq_ignore_ascii_case("tmdb") {
            let index_path =
                people_index_path(&self.config_dir, person_id).map_err(PeopleError::from)?;
            write_atomically(&index_path, &index_bytes).await?;
        }
        Ok(())
    }

    pub(super) async fn person_directory_for_update(
        &self,
        person_name: &str,
        provider: &str,
        person_id: &str,
    ) -> Result<PathBuf, PeopleError> {
        let legacy_dir = people_directory(&self.config_dir, person_name, provider, person_id)
            .map_err(PeopleError::from)?;
        if safe_metadata(&legacy_dir).await?.is_some() {
            return Ok(legacy_dir);
        }

        let mut index_paths = vec![
            people_index_path_for_provider(&self.config_dir, provider, person_id)
                .map_err(PeopleError::from)?,
        ];
        if provider.eq_ignore_ascii_case("tmdb") {
            index_paths
                .push(people_index_path(&self.config_dir, person_id).map_err(PeopleError::from)?);
        }
        for index_path in index_paths {
            let Some(bytes) = read_people_file(&index_path).await? else {
                continue;
            };
            let index = serde_json::from_slice::<StoredPersonIndex>(&bytes)
                .map_err(|source| PeopleError::Serialization(source.to_string()))?;
            if let Some(person_key) = index.person_key.as_deref() {
                if person_key.starts_with("lux-") {
                    return lux_person_directory(&self.config_dir, person_name, person_key)
                        .map_err(PeopleError::from);
                }
                return canonical_person_directory(&self.config_dir, person_key)
                    .map_err(PeopleError::from);
            }
            let relative = Path::new(&index.image_path);
            if relative.is_absolute()
                || relative
                    .components()
                    .any(|component| !matches!(component, Component::Normal(_)))
                || relative.starts_with(Path::new("people/assets"))
            {
                continue;
            }
            let metadata_dir = metadata_root(&self.config_dir);
            let image_path = metadata_dir.join(relative);
            if image_path.starts_with(&metadata_dir)
                && let Some(parent) = image_path.parent()
                && safe_metadata(parent)
                    .await?
                    .is_some_and(|metadata| metadata.is_dir())
            {
                return Ok(parent.to_owned());
            }
        }

        let person_key = person_key_for_identities(&[PersonIdentity {
            provider: provider.to_owned(),
            id: person_id.to_owned(),
        }])
        .ok_or_else(|| PeopleError::InvalidComponent(person_id.to_owned()))?;
        canonical_person_directory(&self.config_dir, &person_key).map_err(PeopleError::from)
    }

    pub async fn profile_image_for_emby_name_or_id(
        &self,
        name_or_id: &str,
    ) -> Result<Option<PersonImage>, PeopleError> {
        if validate_component(name_or_id).is_ok()
            && let Some(image) = self.profile_image(name_or_id).await?
        {
            return Ok(Some(image));
        }
        self.profile_image_for_name(name_or_id).await
    }

    pub(super) async fn profile_image_for_name(
        &self,
        name: &str,
    ) -> Result<Option<PersonImage>, PeopleError> {
        let name = name.trim();
        if name.is_empty()
            || name.len() > 128
            || name.contains('/')
            || name.contains('\\')
            || matches!(name, "." | "..")
        {
            return Err(PeopleError::InvalidComponent(name.to_owned()));
        }
        let people_root = metadata_root(&self.config_dir).join(LEGACY_PEOPLE_DIR);
        let mut buckets = match fs::read_dir(&people_root).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(PeopleError::Io {
                    path: people_root,
                    source,
                });
            }
        };
        let prefix = format!("{}-", readable_component(name));
        while let Some(bucket) = buckets
            .next_entry()
            .await
            .map_err(|source| PeopleError::Io {
                path: people_root.clone(),
                source,
            })?
        {
            let bucket_path = bucket.path();
            if safe_metadata(&bucket_path)
                .await?
                .is_none_or(|metadata| !metadata.is_dir())
            {
                continue;
            }
            let mut persons =
                fs::read_dir(&bucket_path)
                    .await
                    .map_err(|source| PeopleError::Io {
                        path: bucket_path.clone(),
                        source,
                    })?;
            while let Some(person) =
                persons
                    .next_entry()
                    .await
                    .map_err(|source| PeopleError::Io {
                        path: bucket_path.clone(),
                        source,
                    })?
            {
                let person_path = person.path();
                if safe_metadata(&person_path)
                    .await?
                    .is_none_or(|metadata| !metadata.is_dir())
                {
                    continue;
                }
                let Some(person_name) = person.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if !person_name.starts_with(&prefix) {
                    continue;
                }
                for extension in PROFILE_EXTENSIONS {
                    if let Some(image) =
                        image_from_path(&person_path.join(format!("{PERSON_IMAGE}.{extension}")))
                            .await?
                    {
                        return Ok(Some(image));
                    }
                }
            }
        }
        Ok(None)
    }

    pub async fn profile_image_for_provider(
        &self,
        provider: Option<&str>,
        person_id: &str,
    ) -> Result<Option<PersonImage>, PeopleError> {
        validate_component(person_id)?;
        if let Some(provider) = provider {
            validate_component(provider)?;
            if let Some(image) = self
                .indexed_profile_image_for_provider(provider, person_id)
                .await?
            {
                return Ok(Some(image));
            }
            if !provider.eq_ignore_ascii_case("tmdb") {
                return Ok(None);
            }
        } else {
            let legacy_index_path =
                people_index_path(&self.config_dir, person_id).map_err(PeopleError::from)?;
            if let Some(image) = self.read_indexed_person_image(&legacy_index_path).await? {
                return Ok(Some(image));
            }
            let tmdb_index_path =
                people_index_path_for_provider(&self.config_dir, "tmdb", person_id)
                    .map_err(PeopleError::from)?;
            if let Some(image) = self.read_indexed_person_image(&tmdb_index_path).await? {
                return Ok(Some(image));
            }
        }

        let profiles_dir = self.legacy_people_dir().join(LEGACY_PROFILES_DIR);
        for extension in PROFILE_EXTENSIONS {
            let path = profiles_dir.join(format!("{person_id}.{extension}"));
            if let Some(image) = image_from_path(&path).await? {
                return Ok(Some(image));
            }
        }
        Ok(None)
    }

    pub(super) async fn indexed_profile_image_for_provider(
        &self,
        provider: &str,
        person_id: &str,
    ) -> Result<Option<PersonImage>, PeopleError> {
        let index_path = people_index_path_for_provider(&self.config_dir, provider, person_id)
            .map_err(PeopleError::from)?;
        if let Some(image) = self.read_indexed_person_image(&index_path).await? {
            return Ok(Some(image));
        }
        if provider.eq_ignore_ascii_case("tmdb") {
            let legacy_index_path =
                people_index_path(&self.config_dir, person_id).map_err(PeopleError::from)?;
            if let Some(image) = self.read_indexed_person_image(&legacy_index_path).await? {
                return Ok(Some(image));
            }
        }
        Ok(None)
    }

    pub(super) async fn read_indexed_person_image(
        &self,
        index_path: &Path,
    ) -> Result<Option<PersonImage>, PeopleError> {
        let Some(index_bytes) = read_people_file(index_path).await? else {
            return Ok(None);
        };
        let index = serde_json::from_slice::<StoredPersonIndex>(&index_bytes)
            .map_err(|source| PeopleError::Serialization(source.to_string()))?;
        let relative = Path::new(&index.image_path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(PeopleError::Serialization(
                "person image index contains an unsafe path".to_owned(),
            ));
        }
        let metadata_dir = metadata_root(&self.config_dir);
        let path = metadata_dir.join(relative);
        if !path.starts_with(&metadata_dir) {
            return Err(PeopleError::Serialization(
                "person image path is outside metadata".to_owned(),
            ));
        }
        image_from_path(&path).await
    }

    pub(super) fn legacy_people_dir(&self) -> PathBuf {
        self.config_dir.join(LEGACY_PEOPLE_DIR)
    }

    pub(super) async fn ensure_profile_image(
        &self,
        person_id: &str,
        provider: &str,
        image_url: Option<&str>,
        person_dir: &Path,
        prefer_local: bool,
    ) -> Result<Option<String>, PeopleError> {
        if prefer_local {
            for extension in PROFILE_EXTENSIONS {
                let path = person_dir.join(format!("{PERSON_IMAGE}.{extension}"));
                if safe_metadata(&path)
                    .await?
                    .is_some_and(|metadata| metadata.is_file())
                {
                    return Ok(Some(format!("{PERSON_IMAGE}.{extension}")));
                }
            }
        }

        if let Some(image) = self
            .indexed_profile_image_for_provider(provider, person_id)
            .await?
        {
            return self
                .materialize_profile_asset(&image.path, person_dir)
                .await
                .map(Some);
        }

        if !prefer_local {
            for extension in PROFILE_EXTENSIONS {
                let path = person_dir.join(format!("{PERSON_IMAGE}.{extension}"));
                if safe_metadata(&path)
                    .await?
                    .is_some_and(|metadata| metadata.is_file())
                {
                    return Ok(Some(format!("{PERSON_IMAGE}.{extension}")));
                }
            }
        }

        let legacy_profiles_dir = self.legacy_people_dir().join(LEGACY_PROFILES_DIR);
        if provider.eq_ignore_ascii_case("tmdb") {
            for extension in PROFILE_EXTENSIONS {
                let path = legacy_profiles_dir.join(format!("{person_id}.{extension}"));
                if safe_metadata(&path)
                    .await?
                    .is_some_and(|metadata| metadata.is_file())
                {
                    return Ok(Some(format!("legacy/{person_id}.{extension}")));
                }
            }
        }

        // The provider ID is the identity key. If another title already
        // indexed a local image for this person, reuse it instead of
        // downloading the same profile again. The shared bytes are
        // materialized as `folder.<ext>` in this person directory.
        let Some(image_url) = image_url else {
            return Ok(None);
        };

        let url =
            Url::parse(image_url).map_err(|source| PeopleError::InvalidUrl(source.to_string()))?;
        if url.scheme() != "https"
            || url.host_str().is_none_or(str::is_empty)
            || url.path().is_empty()
            || url.port().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(PeopleError::InvalidUrl(
                "actor profile URL must be a valid HTTPS scraper image URL".to_owned(),
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
        let bytes = read_response_body_limited(response, MAX_PROFILE_BYTES as u64)
            .await
            .map_err(|error| match error {
                LimitedBodyError::Download(error) => PeopleError::Download(error),
                LimitedBodyError::TooLarge { .. } => {
                    PeopleError::InvalidImage("profile image payload is invalid".to_owned())
                }
            })?;
        if bytes.is_empty()
            || bytes.len() > MAX_PROFILE_BYTES
            || !valid_image(expected_type, &bytes)
        {
            return Err(PeopleError::InvalidImage(
                "profile image payload is invalid".to_owned(),
            ));
        }
        let image_path = person_dir.join(format!("{PERSON_IMAGE}.{extension}"));
        write_atomically(&image_path, &bytes).await?;
        Ok(Some(format!("{PERSON_IMAGE}.{extension}")))
    }

    pub(super) async fn materialize_profile_asset(
        &self,
        shared_path: &Path,
        person_dir: &Path,
    ) -> Result<String, PeopleError> {
        let Some(metadata) = safe_metadata(shared_path).await? else {
            return Err(PeopleError::Io {
                path: shared_path.to_owned(),
                source: std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "shared profile asset does not exist",
                ),
            });
        };
        if !metadata.is_file() {
            return Err(PeopleError::Io {
                path: shared_path.to_owned(),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "shared profile asset is not a file",
                ),
            });
        }
        let extension = shared_path
            .extension()
            .and_then(|value| value.to_str())
            .filter(|value| PROFILE_EXTENSIONS.contains(value))
            .ok_or_else(|| {
                PeopleError::InvalidImage("shared profile asset extension is invalid".to_owned())
            })?;
        let file_name = format!("{PERSON_IMAGE}.{extension}");
        let target = person_dir.join(&file_name);
        let temporary = person_dir.join(format!(".{file_name}.{}.tmp", Uuid::now_v7()));
        let result = async {
            // Materialize an old shared asset into the current Emby-compatible
            // folder.<ext> layout without copying its bytes.
            fs::hard_link(shared_path, &temporary)
                .await
                .map_err(|source| PeopleError::Io {
                    path: temporary.clone(),
                    source,
                })?;
            fs::rename(&temporary, &target)
                .await
                .map_err(|source| PeopleError::Io {
                    path: target.clone(),
                    source,
                })?;
            Ok(())
        }
        .await;
        if result.is_err() {
            let _ = fs::remove_file(&temporary).await;
        }
        result.map(|()| file_name)
    }
}
