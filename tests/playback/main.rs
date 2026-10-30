mod chapter_detection;
mod chapter_detection_api;
mod chapters;
mod playback;
mod playback_history_archive;
// `playback.rs` also pulls this file in with `#[path = "playback_strm_probe.rs"]`,
// exactly as it did when the file was its own `--test playback_strm_probe` target.
// Both copies are kept so the test count matches the pre-merge layout.
#[allow(clippy::duplicate_mod)]
mod playback_strm_probe;
mod playback_token;
mod playback_webhooks;
mod watch;
mod watch_state_archive;
