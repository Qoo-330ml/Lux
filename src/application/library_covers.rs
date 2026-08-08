use std::{
    collections::HashSet,
    fmt,
    io::Cursor,
    path::{Component, Path, PathBuf},
};

use ab_glyph::{Font, FontArc, FontVec, PxScale, ScaleFont};
use image::{
    DynamicImage, ImageFormat as RasterFormat, Rgba, RgbaImage,
    imageops::{self, FilterType},
};
use imageproc::{
    drawing::draw_text_mut,
    geometric_transformations::{Interpolation, Projection, warp_into},
};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use tokio::{fs, sync::Semaphore};

use crate::{
    application::images::{ImageWriteError, write_image_atomically},
    domain::ids::LibraryId,
    library::LibraryKind,
    storage::{Database, StorageError},
};

pub const MAX_LIBRARY_COVER_BYTES: u64 = 5 * 1024 * 1024;
pub const AUTO_LIBRARY_COVER_TASK_TYPE: &str = "AUTO_LIBRARY_COVER";
pub const AUTO_LIBRARY_COVER_POSTER_COUNT: usize = 9;
const AUTO_LIBRARY_COVER_CANDIDATE_LIMIT: i64 = 64;
const AUTO_LIBRARY_COVER_WIDTH: u32 = 1920;
const AUTO_LIBRARY_COVER_HEIGHT: u32 = 1080;
const SMILEY_SANS_FONT: &[u8] = include_bytes!("../../assets/fonts/SmileySans-Oblique.ttf");

#[derive(Clone)]
pub struct LibraryCoverService {
    database: Database,
    directory: PathBuf,
    font_path: Option<PathBuf>,
    generation_lock: std::sync::Arc<Semaphore>,
}

impl LibraryCoverService {
    pub fn new(database: Database, directory: PathBuf) -> Self {
        Self {
            database,
            directory,
            font_path: configured_cover_font(),
            generation_lock: std::sync::Arc::new(Semaphore::new(1)),
        }
    }

    pub fn with_font_path(mut self, font_path: impl Into<PathBuf>) -> Self {
        self.font_path = Some(font_path.into());
        self
    }

    pub async fn generate_if_eligible(
        &self,
        library_id: LibraryId,
    ) -> Result<AutoLibraryCoverResult, LibraryCoverError> {
        let _permit = self
            .generation_lock
            .acquire()
            .await
            .map_err(|_| LibraryCoverError::GenerationUnavailable)?;
        let library_id_text = library_id.to_string();
        let library = self
            .database
            .find_library(&library_id_text)
            .await?
            .ok_or(LibraryCoverError::LibraryNotFound)?;
        if library.cover_image_path.is_some() {
            return Ok(AutoLibraryCoverResult::ExistingCover);
        }

        let poster_bytes = self.load_posters(&library_id_text).await?;
        if poster_bytes.len() < AUTO_LIBRARY_COVER_POSTER_COUNT {
            return Ok(AutoLibraryCoverResult::BelowThreshold);
        }

        if !self
            .database
            .register_auto_library_cover_task(&library_id_text)
            .await?
        {
            return Ok(AutoLibraryCoverResult::AlreadyHandled);
        }

        if self
            .database
            .find_library(&library_id_text)
            .await?
            .is_some_and(|library| library.cover_image_path.is_some())
        {
            return Ok(AutoLibraryCoverResult::ExistingCover);
        }

        let font_bytes = self.load_font_bytes().await?;
        let library_subtitle = library_kind_subtitle(&library.kind);
        let library_name = library.name;
        let bytes = tokio::task::spawn_blocking(move || {
            render_auto_library_cover(&library_name, library_subtitle, &poster_bytes, &font_bytes)
        })
        .await
        .map_err(|_| LibraryCoverError::RenderPanicked)??;

        if self
            .database
            .find_library(&library_id_text)
            .await?
            .is_some_and(|library| library.cover_image_path.is_some())
        {
            return Ok(AutoLibraryCoverResult::ExistingCover);
        }
        match self.store_generated(library_id, &bytes, false).await {
            Ok(_) => Ok(AutoLibraryCoverResult::Generated),
            Err(LibraryCoverError::GeneratedCoverRace) => Ok(AutoLibraryCoverResult::ExistingCover),
            Err(error) => Err(error),
        }
    }

    pub async fn run_manually(
        &self,
        library_id: LibraryId,
    ) -> Result<AutoLibraryCoverResult, LibraryCoverError> {
        let _permit = self
            .generation_lock
            .acquire()
            .await
            .map_err(|_| LibraryCoverError::GenerationUnavailable)?;
        let library_id_text = library_id.to_string();
        let library = self
            .database
            .find_library(&library_id_text)
            .await?
            .ok_or(LibraryCoverError::LibraryNotFound)?;
        if library
            .cover_image_path
            .as_deref()
            .is_some_and(|path| path != format!("{library_id_text}-auto.jpg"))
        {
            return Ok(AutoLibraryCoverResult::ExistingCover);
        }
        if self
            .database
            .find_scheduled_task_config("LIBRARY", &library_id_text, AUTO_LIBRARY_COVER_TASK_TYPE)
            .await?
            .is_none()
        {
            return Ok(AutoLibraryCoverResult::TaskNotRegistered);
        }

        let poster_bytes = self.load_posters(&library_id_text).await?;
        if poster_bytes.len() < AUTO_LIBRARY_COVER_POSTER_COUNT {
            return Ok(AutoLibraryCoverResult::BelowThreshold);
        }
        let font_bytes = self.load_font_bytes().await?;
        let library_subtitle = library_kind_subtitle(&library.kind);
        let library_name = library.name;
        let bytes = tokio::task::spawn_blocking(move || {
            render_auto_library_cover(&library_name, library_subtitle, &poster_bytes, &font_bytes)
        })
        .await
        .map_err(|_| LibraryCoverError::RenderPanicked)??;

        match self.store_generated(library_id, &bytes, true).await {
            Ok(_) => Ok(AutoLibraryCoverResult::Generated),
            Err(LibraryCoverError::GeneratedCoverRace) => Ok(AutoLibraryCoverResult::ExistingCover),
            Err(error) => Err(error),
        }
    }

    async fn load_posters(&self, library_id: &str) -> Result<Vec<Vec<u8>>, LibraryCoverError> {
        let candidates = self
            .database
            .list_random_library_poster_paths(library_id, AUTO_LIBRARY_COVER_CANDIDATE_LIMIT)
            .await?;
        let mut poster_bytes = Vec::with_capacity(AUTO_LIBRARY_COVER_POSTER_COUNT);
        let mut seen_items = HashSet::new();
        for candidate in candidates {
            if !seen_items.insert(candidate.item_id) {
                continue;
            }
            let Ok(root_path) = fs::canonicalize(&candidate.root_path).await else {
                continue;
            };
            let Ok(poster_path) = fs::canonicalize(&candidate.local_path).await else {
                continue;
            };
            if poster_path == root_path || !poster_path.starts_with(&root_path) {
                continue;
            }
            let Ok(bytes) = fs::read(&poster_path).await else {
                continue;
            };
            poster_bytes.push(bytes);
            if poster_bytes.len() == AUTO_LIBRARY_COVER_POSTER_COUNT {
                break;
            }
        }
        Ok(poster_bytes)
    }

    async fn load_font_bytes(&self) -> Result<Vec<u8>, LibraryCoverError> {
        let Some(font_path) = self.font_path.as_ref() else {
            return Ok(bundled_cover_font().to_vec());
        };
        fs::read(font_path)
            .await
            .map_err(|source| image_io_error(font_path, source))
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

    async fn store_generated(
        &self,
        library_id: LibraryId,
        bytes: &[u8],
        allow_existing_auto: bool,
    ) -> Result<StoredLibraryCover, LibraryCoverError> {
        validate_payload(ImageFormat::Jpeg, bytes)?;
        let byte_length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if byte_length > MAX_LIBRARY_COVER_BYTES {
            return Err(LibraryCoverError::TooLarge {
                size: byte_length,
                max: MAX_LIBRARY_COVER_BYTES,
            });
        }
        let library_id_text = library_id.to_string();
        let library = self
            .database
            .find_library(&library_id_text)
            .await?
            .ok_or(LibraryCoverError::LibraryNotFound)?;
        if !allow_existing_auto && library.cover_image_path.is_some() {
            return Err(LibraryCoverError::GeneratedCoverRace);
        }
        let file_name = format!("{library_id_text}-auto.jpg");
        fs::create_dir_all(&self.directory)
            .await
            .map_err(|source| image_io_error(&self.directory, source))?;
        let target = self.directory.join(&file_name);
        write_image_atomically(&target, bytes).await?;
        let size = i64::try_from(bytes.len()).map_err(|_| LibraryCoverError::TooLarge {
            size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            max: i64::MAX as u64,
        })?;
        let tag = content_tag(bytes);
        let updated = if allow_existing_auto {
            self.database
                .update_library_cover_if_auto(
                    &library_id_text,
                    &file_name,
                    ImageFormat::Jpeg.content_type(),
                    size,
                    &tag,
                )
                .await?
        } else {
            self.database
                .update_library_cover_if_missing(
                    &library_id_text,
                    &file_name,
                    ImageFormat::Jpeg.content_type(),
                    size,
                    &tag,
                )
                .await?
        };
        if !updated {
            let _ = fs::remove_file(&target).await;
            return Err(LibraryCoverError::GeneratedCoverRace);
        }
        Ok(StoredLibraryCover {
            path: target,
            content_type: ImageFormat::Jpeg.content_type().to_owned(),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutoLibraryCoverResult {
    BelowThreshold,
    ExistingCover,
    TaskNotRegistered,
    AlreadyHandled,
    Generated,
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
    FontNotFound,
    Render(String),
    RenderPanicked,
    GeneratedCoverRace,
    GenerationUnavailable,
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
            Self::FontNotFound => formatter.write_str("library cover font was not found"),
            Self::Render(error) => write!(formatter, "library cover render failed: {error}"),
            Self::RenderPanicked => formatter.write_str("library cover render task failed"),
            Self::GeneratedCoverRace => {
                formatter.write_str("library cover was set while auto cover was rendering")
            }
            Self::GenerationUnavailable => {
                formatter.write_str("library cover generation is unavailable")
            }
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
            | Self::LibraryNotFound
            | Self::FontNotFound
            | Self::Render(_)
            | Self::RenderPanicked
            | Self::GeneratedCoverRace
            | Self::GenerationUnavailable => None,
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

fn configured_cover_font() -> Option<PathBuf> {
    std::env::var_os("LUX_COVER_FONT_PATH").map(PathBuf::from)
}

fn bundled_cover_font() -> &'static [u8] {
    SMILEY_SANS_FONT
}

fn render_auto_library_cover(
    library_name: &str,
    library_subtitle: &str,
    poster_bytes: &[Vec<u8>],
    font_bytes: &[u8],
) -> Result<Vec<u8>, LibraryCoverError> {
    let mut posters = poster_bytes
        .iter()
        .filter_map(|bytes| image::load_from_memory(bytes).ok())
        .collect::<Vec<_>>();
    if posters.len() < AUTO_LIBRARY_COVER_POSTER_COUNT {
        return Err(LibraryCoverError::Render(
            "fewer than nine valid poster images were available".to_owned(),
        ));
    }
    shuffle(&mut posters);
    posters.truncate(AUTO_LIBRARY_COVER_POSTER_COUNT);

    let mut canvas = gradient_background(&posters[0]);
    let mut processed = posters
        .iter()
        .map(|poster| {
            let card = fit_cover(poster, 410, 610);
            add_shadow(&rounded_corners(card, 46), 15, 15, 15.0)
        })
        .collect::<Vec<_>>();

    let card_w = processed[0].width();
    let card_h = processed[0].height();
    let stack_gap = -30_i32;
    let margin_y = 22_u32;
    let column_height = card_h
        .saturating_mul(3)
        .saturating_add(margin_y.saturating_mul(3));
    for column in 0..3 {
        let mut stack = RgbaImage::new(card_w, column_height);
        for row in 0..3 {
            let index = column * 3 + row;
            let y = i64::from(row as u32 * card_h) + i64::from(stack_gap * row as i32);
            overlay(&mut stack, &processed[index], 0, y as i32);
        }
        let rotated = rotate_cover_column(&stack);
        let x = 350 + column as i32 * (410 - 50 + 50);
        let y = -200 - column as i32 * 80;
        overlay(&mut canvas, &rotated, x, y);
    }
    processed.clear();

    let font = FontArc::try_from_vec(font_bytes.to_vec())
        .or_else(|_| FontVec::try_from_vec_and_index(font_bytes.to_vec(), 0).map(Into::into))
        .map_err(|_| LibraryCoverError::Render("cover font could not be loaded".to_owned()))?;
    draw_library_text(&mut canvas, library_name, library_subtitle, &font);

    let mut output = Vec::new();
    DynamicImage::ImageRgba8(canvas)
        .write_to(&mut Cursor::new(&mut output), RasterFormat::Jpeg)
        .map_err(|error| LibraryCoverError::Render(error.to_string()))?;
    Ok(output)
}

fn library_kind_subtitle(kind: &str) -> &'static str {
    match kind.parse::<LibraryKind>() {
        Ok(LibraryKind::Movie) => "Movies",
        Ok(LibraryKind::Series) => "Series",
        Ok(LibraryKind::Mixed) => "Mixed",
        Err(_) => "Media",
    }
}

fn rotate_cover_column(column: &RgbaImage) -> RgbaImage {
    // Pillow's rotate(-16, expand=True) is clockwise; imageproc uses clockwise-positive radians.
    let theta = 16.0_f32.to_radians();
    let (sin_theta, cos_theta) = theta.sin_cos();
    let output_width = (column.width() as f32 * cos_theta.abs()
        + column.height() as f32 * sin_theta.abs())
    .ceil() as u32;
    let output_height = (column.height() as f32 * cos_theta.abs()
        + column.width() as f32 * sin_theta.abs())
    .ceil() as u32;
    let projection = Projection::translate(output_width as f32 / 2.0, output_height as f32 / 2.0)
        * Projection::rotate(theta)
        * Projection::translate(
            -(column.width() as f32) / 2.0,
            -(column.height() as f32) / 2.0,
        );
    let mut rotated = RgbaImage::new(output_width, output_height);
    warp_into(
        column,
        &projection,
        Interpolation::Bicubic,
        Rgba([0, 0, 0, 0]),
        &mut rotated,
    );
    rotated
}

fn gradient_background(first_poster: &DynamicImage) -> RgbaImage {
    let sample_image = first_poster.resize(1, 1, FilterType::Triangle).to_rgb8();
    let sample = *sample_image.get_pixel(0, 0);
    let left = [
        (u16::from(sample[0]) * 3 / 5) as u8,
        (u16::from(sample[1]) * 3 / 5) as u8,
        (u16::from(sample[2]) * 3 / 5) as u8,
    ];
    let right = [
        sample[0].saturating_add(sample[0] / 5),
        sample[1].saturating_add(sample[1] / 5),
        sample[2].saturating_add(sample[2] / 5),
    ];
    let mut background = RgbaImage::new(AUTO_LIBRARY_COVER_WIDTH, AUTO_LIBRARY_COVER_HEIGHT);
    for x in 0..AUTO_LIBRARY_COVER_WIDTH {
        let ratio = x as f32 / (AUTO_LIBRARY_COVER_WIDTH - 1) as f32;
        let color = Rgba([
            lerp(left[0], right[0], ratio),
            lerp(left[1], right[1], ratio),
            lerp(left[2], right[2], ratio),
            255,
        ]);
        for y in 0..AUTO_LIBRARY_COVER_HEIGHT {
            background.put_pixel(x, y, color);
        }
    }
    background
}

fn lerp(left: u8, right: u8, ratio: f32) -> u8 {
    (f32::from(left) * (1.0 - ratio) + f32::from(right) * ratio)
        .round()
        .clamp(0.0, 255.0) as u8
}

fn fit_cover(image: &DynamicImage, width: u32, height: u32) -> RgbaImage {
    let scale = (width as f32 / image.width() as f32).max(height as f32 / image.height() as f32);
    let resized_width = (image.width() as f32 * scale).ceil() as u32;
    let resized_height = (image.height() as f32 * scale).ceil() as u32;
    let resized = imageops::resize(
        &image.to_rgba8(),
        resized_width,
        resized_height,
        FilterType::Lanczos3,
    );
    let x = resized.width().saturating_sub(width) / 2;
    let y = resized.height().saturating_sub(height) / 2;
    imageops::crop_imm(&resized, x, y, width, height).to_image()
}

fn rounded_corners(mut image: RgbaImage, radius: u32) -> RgbaImage {
    let radius = radius.min(image.width() / 2).min(image.height() / 2);
    let radius_squared = i64::from(radius) * i64::from(radius);
    for y in 0..image.height() {
        for x in 0..image.width() {
            let dx = if x < radius {
                i64::from(radius - x)
            } else if x >= image.width() - radius {
                i64::from(x - (image.width() - radius - 1))
            } else {
                0
            };
            let dy = if y < radius {
                i64::from(radius - y)
            } else if y >= image.height() - radius {
                i64::from(y - (image.height() - radius - 1))
            } else {
                0
            };
            if dx > 0 && dy > 0 && dx * dx + dy * dy > radius_squared {
                image.get_pixel_mut(x, y).0[3] = 0;
            }
        }
    }
    image
}

fn add_shadow(image: &RgbaImage, offset_x: i32, offset_y: i32, blur: f32) -> RgbaImage {
    let blur_px = blur.ceil() as u32;
    let width = image.width() + offset_x.unsigned_abs() + blur_px * 2;
    let height = image.height() + offset_y.unsigned_abs() + blur_px * 2;
    let mut shadow = RgbaImage::new(width, height);
    for (x, y, pixel) in image.enumerate_pixels() {
        let alpha = u16::from(pixel.0[3]) * 180 / 255;
        let shadow_x = blur_px + offset_x.max(0) as u32 + x;
        let shadow_y = blur_px + offset_y.max(0) as u32 + y;
        shadow.put_pixel(shadow_x, shadow_y, Rgba([0, 0, 0, alpha as u8]));
    }
    let shadow = imageops::fast_blur(&shadow, blur);
    let mut result = shadow;
    overlay(
        &mut result,
        image,
        blur_px as i32 + (-offset_x).max(0),
        blur_px as i32 + (-offset_y).max(0),
    );
    result
}

fn overlay(base: &mut RgbaImage, layer: &RgbaImage, left: i32, top: i32) {
    for y in 0..layer.height() {
        for x in 0..layer.width() {
            let target_x = left + x as i32;
            let target_y = top + y as i32;
            if target_x < 0
                || target_y < 0
                || target_x >= base.width() as i32
                || target_y >= base.height() as i32
            {
                continue;
            }
            let source = layer.get_pixel(x, y);
            if source.0[3] == 0 {
                continue;
            }
            let destination = base.get_pixel_mut(target_x as u32, target_y as u32);
            let source_alpha = u32::from(source.0[3]);
            let destination_alpha = u32::from(destination.0[3]);
            let output_alpha = source_alpha + destination_alpha * (255 - source_alpha) / 255;
            if output_alpha == 0 {
                continue;
            }
            for channel in 0..3 {
                destination.0[channel] = ((u32::from(source.0[channel]) * source_alpha
                    + u32::from(destination.0[channel]) * destination_alpha * (255 - source_alpha)
                        / 255)
                    / output_alpha) as u8;
            }
            destination.0[3] = output_alpha as u8;
        }
    }
}

fn draw_library_text(canvas: &mut RgbaImage, name: &str, subtitle: &str, font: &FontArc) {
    let scale = PxScale::from(160.0);
    let lines = wrap_text(font, scale, name, 960.0);
    let line_height = font.as_scaled(scale).height().ceil() as i32;
    draw_accent_bar(canvas, 113, 626, 20, 100);
    for (index, line) in lines.iter().enumerate() {
        let y = 432 + index as i32 * line_height;
        draw_text_mut(canvas, Rgba([0, 0, 0, 100]), 101, y + 5, scale, font, line);
        draw_text_mut(canvas, Rgba([255, 255, 255, 255]), 96, y, scale, font, line);
    }
    let subtitle_scale = PxScale::from(50.0);
    draw_text_mut(
        canvas,
        Rgba([0, 0, 0, 100]),
        156,
        629,
        subtitle_scale,
        font,
        subtitle,
    );
    draw_text_mut(
        canvas,
        Rgba([255, 255, 255, 255]),
        153,
        626,
        subtitle_scale,
        font,
        subtitle,
    );
}

fn draw_accent_bar(canvas: &mut RgbaImage, left: i32, top: i32, width: u32, height: u32) {
    let color = random_bright_color();
    for y in 0..height {
        for x in 0..width {
            let target_x = left + x as i32;
            let target_y = top + y as i32;
            if target_x >= 0
                && target_y >= 0
                && target_x < canvas.width() as i32
                && target_y < canvas.height() as i32
            {
                canvas.put_pixel(target_x as u32, target_y as u32, color);
            }
        }
    }
}

fn random_bright_color() -> Rgba<u8> {
    let hue = (OsRng.next_u32() % 360) as f32 / 360.0;
    let saturation = 0.5 + (OsRng.next_u32() % 500) as f32 / 1000.0;
    let value = 0.7 + (OsRng.next_u32() % 300) as f32 / 1000.0;
    let channel = |n: f32| -> u8 { (n.clamp(0.0, 1.0) * 255.0).round() as u8 };
    let hue_sector = hue * 6.0;
    let sector = hue_sector.floor();
    let fraction = hue_sector - sector;
    let p = value * (1.0 - saturation);
    let q = value * (1.0 - saturation * fraction);
    let t = value * (1.0 - saturation * (1.0 - fraction));
    let (red, green, blue) = match sector as u32 {
        0 => (value, t, p),
        1 => (q, value, p),
        2 => (p, value, t),
        3 => (p, q, value),
        4 => (t, p, value),
        _ => (value, p, q),
    };
    Rgba([channel(red), channel(green), channel(blue), 255])
}

fn wrap_text(font: &FontArc, scale: PxScale, text: &str, max_width: f32) -> Vec<String> {
    let scaled = font.as_scaled(scale);
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut width = 0.0_f32;
    for character in text.chars() {
        let glyph = scaled.glyph_id(character);
        let advance = scaled.h_advance(glyph);
        if !current.is_empty() && width + advance > max_width {
            lines.push(current);
            current = String::new();
            width = 0.0;
        }
        current.push(character);
        width += advance;
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn shuffle<T>(items: &mut [T]) {
    for index in (1..items.len()).rev() {
        let swap_index = (OsRng.next_u32() as usize) % (index + 1);
        items.swap(index, swap_index);
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

#[cfg(test)]
mod tests {
    use image::Rgba;

    use super::{
        RgbaImage, add_shadow, bundled_cover_font, library_kind_subtitle, rotate_cover_column,
    };

    #[test]
    fn bundled_cover_font_is_smiley_sans() {
        let font = bundled_cover_font();
        assert!(font.len() > 2_000_000);
        assert!(font.starts_with(&[0x00, 0x01, 0x00, 0x00]));
    }

    #[test]
    fn library_kind_uses_english_cover_subtitle() {
        assert_eq!(library_kind_subtitle("MOVIE"), "Movies");
        assert_eq!(library_kind_subtitle("SERIES"), "Series");
        assert_eq!(library_kind_subtitle("MIXED"), "Mixed");
    }

    #[test]
    fn shadow_canvas_matches_python_layout() {
        let image = RgbaImage::new(410, 610);

        let shadowed = add_shadow(&image, 15, 15, 15.0);

        assert_eq!((shadowed.width(), shadowed.height()), (455, 655));
    }

    #[test]
    fn cover_column_rotation_matches_pillow_expand_and_direction() {
        let mut column = RgbaImage::new(455, 2021);
        for y in 0..20 {
            for x in 218..238 {
                column.put_pixel(x, y, Rgba([255, 255, 255, 255]));
            }
        }

        let rotated = rotate_cover_column(&column);

        assert_eq!((rotated.width(), rotated.height()), (995, 2069));
        let visible_x = rotated
            .enumerate_pixels()
            .filter(|(_, _, pixel)| pixel[3] > 0)
            .map(|(x, _, _)| u64::from(x))
            .collect::<Vec<_>>();
        assert!(!visible_x.is_empty());
        assert!(visible_x.iter().sum::<u64>() / visible_x.len() as u64 > 497);
    }
}
