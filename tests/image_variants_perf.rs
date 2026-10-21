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

/// Peak resident memory while two generations run at once.
///
/// The production cache generates with a semaphore of two, and the plan budgets
/// under 40 MB of growth at that concurrency. `ru_maxrss` is a high-water mark
/// for the whole process, so the test warms up first and reports the delta from
/// the warm baseline.
#[test]
#[ignore = "requires LUX_PERF_IMAGE_DIR with real images; run explicitly"]
fn image_variant_concurrent_memory() {
    const CONCURRENCY: usize = 2;

    let root = std::env::var_os("LUX_PERF_IMAGE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("media"));
    let files = image_files(&root);
    assert!(
        !files.is_empty(),
        "no supported images under {}; set LUX_PERF_IMAGE_DIR",
        root.display()
    );
    // The plan's 40 MB budget assumes a typical poster decode (about 14 MB per
    // image). Keep the middle of the size distribution, so a 4K still or a
    // 11 MB clearlogo does not stand in for the poster workload.
    let mut sized: Vec<(u64, PathBuf)> = files
        .iter()
        .filter_map(|path| {
            let size = std::fs::metadata(path).ok()?.len();
            Some((size, path.clone()))
        })
        .collect();
    sized.sort_by_key(|(size, _)| *size);
    let low = sized.len() * 40 / 100;
    let high = (sized.len() * 60 / 100).max(low + 1);
    let sample: Vec<PathBuf> = sized[low..high]
        .iter()
        .map(|(_, path)| path.clone())
        .collect();

    let workspace = tempfile::tempdir().expect("temporary output directory");
    let peak = || -> u64 {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        // SAFETY: getrusage only writes into the provided rusage buffer.
        let status = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
        assert_eq!(status, 0, "getrusage failed");
        // SAFETY: the call above succeeded, so the buffer is initialised.
        let usage = unsafe { usage.assume_init() };
        #[cfg(target_os = "macos")]
        {
            usage.ru_maxrss as u64
        }
        #[cfg(not(target_os = "macos"))]
        {
            usage.ru_maxrss as u64 * 1024
        }
    };

    let warm_target = workspace.path().join("warm.jpg");
    generate(&sample[0], &warm_target, 1280).expect("warm-up generation");
    let baseline = peak();

    // One round only: `ru_maxrss` is a process high-water mark, so repeated
    // rounds would keep reporting the first allocation instead of the steady
    // state. Two concurrent generations are exactly the production semaphore.
    std::thread::scope(|scope| {
        for slot in 0..CONCURRENCY {
            let source = &sample[slot % sample.len()];
            let target = workspace.path().join(format!("r-{slot}.jpg"));
            scope.spawn(move || {
                generate(source, &target, 1280).expect("variant generation");
            });
        }
    });

    let after = peak();
    let growth_mb = after.saturating_sub(baseline) as f64 / (1024.0 * 1024.0);
    eprintln!(
        "concurrency={CONCURRENCY} baseline={} B peak={} B growth={growth_mb:.1} MB",
        baseline, after
    );
    assert!(
        growth_mb < 40.0,
        "concurrent generation grew peak memory by {growth_mb:.1} MB, budget is 40 MB"
    );
}
