//! Ignored benchmark for the display-sized image variant pipeline.
//!
//! Run with a real image directory:
//! `LUX_PERF_IMAGE_DIR=media cargo test --release --locked --test image_variants_perf -- --ignored --nocapture`

use std::{
    io::Cursor,
    path::{Path, PathBuf},
    time::Instant,
};

use image::{
    DynamicImage, ExtendedColorType, ImageDecoder, ImageReader, codecs::jpeg::JpegEncoder,
    imageops::FilterType,
};

const SAMPLE_COUNT: usize = 1_000;
const WIDTHS: [u32; 3] = [360, 720, 1280];
const JPEG_QUALITY: u8 = 80;

fn image_files(root: &Path) -> Vec<PathBuf> {
    fn walk(root: &Path, files: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(root) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, files);
            } else if matches!(
                path.extension()
                    .and_then(|value| value.to_str())
                    .map(str::to_ascii_lowercase)
                    .as_deref(),
                Some("jpg" | "jpeg" | "png" | "webp")
            ) {
                files.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(root, &mut files);
    files.sort();
    files
}

fn generate(
    source: &Path,
    target: &Path,
    target_width: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = std::fs::read(source)?;
    let reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut decoder = reader.into_decoder()?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    if image.width().max(image.height()) <= target_width {
        return Ok(());
    }
    let resized = image.resize(target_width, target_width, FilterType::Lanczos3);
    let rgb = resized.to_rgb8();
    let mut encoded = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut encoded, JPEG_QUALITY);
    encoder.encode(
        rgb.as_raw(),
        rgb.width(),
        rgb.height(),
        ExtendedColorType::Rgb8,
    )?;
    std::fs::write(target, encoded)?;
    Ok(())
}

fn percentile(samples: &[f64], fraction: f64) -> f64 {
    let sorted = {
        let mut values = samples.to_vec();
        values.sort_by(f64::total_cmp);
        values
    };
    let index = ((sorted.len() as f64 * fraction).ceil() as usize).saturating_sub(1);
    sorted[index.min(sorted.len() - 1)]
}

#[test]
#[ignore = "requires LUX_PERF_IMAGE_DIR with real images; run explicitly"]
fn image_variant_generation_latency() {
    let root = std::env::var_os("LUX_PERF_IMAGE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("media"));
    let files = image_files(&root);
    assert!(
        !files.is_empty(),
        "no supported images under {}; set LUX_PERF_IMAGE_DIR",
        root.display()
    );
    let workspace = tempfile::tempdir().expect("temporary output directory");
    eprintln!(
        "images={} unique={} samples_per_width={SAMPLE_COUNT} root={}",
        files.len(),
        files.len().min(SAMPLE_COUNT),
        root.display()
    );
    for width in WIDTHS {
        let started = Instant::now();
        let mut samples = Vec::with_capacity(SAMPLE_COUNT);
        for index in 0..SAMPLE_COUNT {
            let source = &files[index % files.len()];
            let target = workspace.path().join(format!("{width}-{index}.jpg"));
            let sample_start = Instant::now();
            generate(source, &target, width).expect("variant generation");
            samples.push(sample_start.elapsed().as_secs_f64() * 1_000.0);
        }
        let total = started.elapsed().as_secs_f64();
        eprintln!(
            "width={width} total={total:.2}s p50={:.2}ms p95={:.2}ms max={:.2}ms",
            percentile(&samples, 0.50),
            percentile(&samples, 0.95),
            percentile(&samples, 1.0),
        );
        assert!(
            percentile(&samples, 0.95) <= 120.0,
            "width={width} p95 exceeded 120ms"
        );
    }
}
