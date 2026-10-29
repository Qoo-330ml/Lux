//! Administrator-controlled server transcoding policy.
//!
//! The values here mirror what used to be compile-time constants or environment-only switches in
//! `application::playback::hls`. They are persisted as one JSON row in `server_settings`, validated
//! before they are written, and applied to every newly started HLS session. Environment variables
//! stay available as operator overrides for the two deployment-sensitive values (hardware encoder
//! and the global disk budget) and win over the stored policy.

use serde::{Deserialize, Serialize};

use crate::storage::Database;

/// Sentinels for "no hardware encoder" in the stored policy.
pub(crate) const HARDWARE_ENCODER_NONE: &str = "NONE";
/// Encoders Lux knows how to ask FFmpeg for. Mirrors the allowlist enforced at runtime.
pub(crate) const HARDWARE_ENCODERS: [&str; 4] =
    ["h264_nvenc", "h264_qsv", "h264_vaapi", "h264_videotoolbox"];
/// Hardware encoders that look usable on this host, probed once from device nodes only (no
/// process is spawned). The saved self-test remains the authority on whether one really works.
pub(crate) fn detected_hardware_encoders() -> &'static [&'static str] {
    static DETECTED: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    DETECTED.get_or_init(detect_hardware_encoders).as_slice()
}

fn detect_hardware_encoders() -> Vec<&'static str> {
    let mut found = Vec::new();
    if cfg!(target_os = "macos") {
        found.push("h264_videotoolbox");
        return found;
    }
    if std::path::Path::new("/dev/nvidiactl").exists() {
        found.push("h264_nvenc");
    }
    let nodes: Vec<String> = std::fs::read_dir("/dev/dri")
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.starts_with("renderD"))
                .collect()
        })
        .unwrap_or_default();
    if !nodes.is_empty() {
        let intel = nodes.iter().any(|node| {
            std::fs::read_to_string(format!("/sys/class/drm/{node}/device/vendor"))
                .map(|vendor| vendor.trim() == "0x8086")
                .unwrap_or(false)
        });
        if intel {
            found.push("h264_qsv");
        }
        found.push("h264_vaapi");
    }
    found
}

/// Encoders to offer in the admin UI: detected ones, plus the saved choice so it stays visible.
pub(crate) fn offered_hardware_encoders(current: &str) -> Vec<&'static str> {
    let mut offered: Vec<&'static str> = detected_hardware_encoders().to_vec();
    if let Some(saved) = HARDWARE_ENCODERS
        .iter()
        .find(|known| known.eq_ignore_ascii_case(current))
    {
        if !offered.contains(saved) {
            offered.push(saved);
        }
    }
    offered
}
/// libx264 presets offered in the admin UI; the runtime only accepts these.
pub(crate) const SOFTWARE_PRESETS: [&str; 6] = [
    "ultrafast",
    "superfast",
    "veryfast",
    "faster",
    "fast",
    "medium",
];
/// Default DRM render node used for VAAPI and QSV when no device is configured.
pub(crate) const DEFAULT_HARDWARE_DEVICE: &str = "/dev/dri/renderD128";

const MAX_SESSIONS_PER_TIER: i64 = 16;
const MAX_SESSION_QUOTA_MB: i64 = 65_536;
const MAX_GLOBAL_BUDGET_MB: i64 = 1_048_576;
const MAX_VIDEO_BITRATE_KBPS: i64 = 100_000;
const MIN_AUDIO_BITRATE_KBPS: i64 = 32;
const MAX_AUDIO_BITRATE_KBPS: i64 = 512;
const MAX_OUTPUT_HEIGHT: i64 = 2_160;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TranscodingSettings {
    /// When off, local media only plays directly (or through a browser-side fallback).
    #[serde(default = "default_enabled")]
    pub(crate) enabled: bool,
    /// `NONE` or one of [`HARDWARE_ENCODERS`]; `LUX_HLS_HW_ENCODER` overrides it at runtime.
    #[serde(default = "default_hardware_encoder")]
    pub(crate) hardware_encoder: String,
    /// Tier 1 (remux) and tier 2 (audio transcode) slots; 0 disables both tiers.
    #[serde(default = "default_remux_sessions")]
    pub(crate) remux_sessions: i64,
    /// Tier 3 slots; 0 disables hardware transcoding.
    #[serde(default = "default_hardware_sessions")]
    pub(crate) hardware_sessions: i64,
    /// Tier 4 slots; 0 disables software transcoding.
    #[serde(default = "default_software_sessions")]
    pub(crate) software_sessions: i64,
    /// Per-session HLS directory quota; 0 means unlimited.
    #[serde(default = "default_session_quota_mb")]
    pub(crate) session_quota_mb: i64,
    /// Cap for all HLS session directories together; 0 means unlimited.
    #[serde(default = "default_global_budget_mb")]
    pub(crate) global_budget_mb: i64,
    /// Refuse to start when the playback volume has less free space than this; 0 disables the guard.
    #[serde(default = "default_min_free_space_mb")]
    pub(crate) min_free_space_mb: i64,
    /// Upper bound for the encoded video bitrate; 0 leaves the encoder default.
    #[serde(default)]
    pub(crate) max_video_bitrate_kbps: i64,
    /// `libx264` preset for software transcoding.
    #[serde(default = "default_software_preset")]
    pub(crate) software_preset: String,
    /// Scale transcoded output down to this height; 0 keeps the source resolution.
    #[serde(default)]
    pub(crate) max_output_height: i64,
    /// AAC bitrate for audio transcoding.
    #[serde(default = "default_audio_bitrate_kbps")]
    pub(crate) audio_bitrate_kbps: i64,
    /// DRM render node for VAAPI/QSV; empty keeps the FFmpeg default.
    #[serde(default = "default_hardware_device")]
    pub(crate) hardware_device: String,
    /// Let FFmpeg decode on the GPU before encoding; failures fall back to the software tier.
    #[serde(default)]
    pub(crate) hardware_decode: bool,
}

fn default_enabled() -> bool {
    true
}

fn default_hardware_encoder() -> String {
    HARDWARE_ENCODER_NONE.to_owned()
}

fn default_remux_sessions() -> i64 {
    4
}

fn default_hardware_sessions() -> i64 {
    2
}

fn default_software_sessions() -> i64 {
    1
}

fn default_session_quota_mb() -> i64 {
    2_048
}

fn default_global_budget_mb() -> i64 {
    8_192
}

fn default_min_free_space_mb() -> i64 {
    512
}

fn default_software_preset() -> String {
    "veryfast".to_owned()
}

fn default_audio_bitrate_kbps() -> i64 {
    192
}

fn default_hardware_device() -> String {
    DEFAULT_HARDWARE_DEVICE.to_owned()
}

impl Default for TranscodingSettings {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            hardware_encoder: default_hardware_encoder(),
            remux_sessions: default_remux_sessions(),
            hardware_sessions: default_hardware_sessions(),
            software_sessions: default_software_sessions(),
            session_quota_mb: default_session_quota_mb(),
            global_budget_mb: default_global_budget_mb(),
            min_free_space_mb: default_min_free_space_mb(),
            max_video_bitrate_kbps: 0,
            software_preset: default_software_preset(),
            max_output_height: 0,
            audio_bitrate_kbps: default_audio_bitrate_kbps(),
            hardware_device: default_hardware_device(),
            hardware_decode: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TranscodingSettingsError {
    SessionsOutOfRange,
    QuotaOutOfRange,
    BitrateOutOfRange,
    UnknownPreset,
    UnknownEncoder,
    OutputHeightOutOfRange,
    DeviceUnusable,
}

impl std::fmt::Display for TranscodingSettingsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::SessionsOutOfRange => "invalid transcoding concurrency limit",
            Self::QuotaOutOfRange => "invalid transcoding disk quota",
            Self::BitrateOutOfRange => "invalid transcoding bitrate",
            Self::UnknownPreset => "invalid software encoding preset",
            Self::UnknownEncoder => "invalid hardware encoder",
            Self::OutputHeightOutOfRange => "invalid maximum output height",
            Self::DeviceUnusable => "invalid hardware device path",
        })
    }
}

impl std::error::Error for TranscodingSettingsError {}

impl TranscodingSettings {
    /// Returns the canonical form: trimmed encoder/preset casing, `NONE` for "no encoder".
    pub(crate) fn normalized(mut self) -> Result<Self, TranscodingSettingsError> {
        let encoder = self.hardware_encoder.trim();
        let encoder = if encoder.is_empty() || encoder.eq_ignore_ascii_case("auto") {
            // "auto"/empty historically meant "no explicit hardware encoder".
            HARDWARE_ENCODER_NONE.to_owned()
        } else if encoder.eq_ignore_ascii_case(HARDWARE_ENCODER_NONE) {
            HARDWARE_ENCODER_NONE.to_owned()
        } else {
            let lower = encoder.to_ascii_lowercase();
            if !HARDWARE_ENCODERS.contains(&lower.as_str()) {
                return Err(TranscodingSettingsError::UnknownEncoder);
            }
            lower
        };
        self.hardware_encoder = encoder;

        let device = self.hardware_device.trim().to_owned();
        if device.chars().count() > 128 || device.chars().any(char::is_control) {
            return Err(TranscodingSettingsError::DeviceUnusable);
        }
        self.hardware_device = if device.is_empty() {
            DEFAULT_HARDWARE_DEVICE.to_owned()
        } else {
            device
        };

        let preset = self.software_preset.trim().to_ascii_lowercase();
        if !SOFTWARE_PRESETS.contains(&preset.as_str()) {
            return Err(TranscodingSettingsError::UnknownPreset);
        }
        self.software_preset = preset;

        for sessions in [
            self.remux_sessions,
            self.hardware_sessions,
            self.software_sessions,
        ] {
            if !(0..=MAX_SESSIONS_PER_TIER).contains(&sessions) {
                return Err(TranscodingSettingsError::SessionsOutOfRange);
            }
        }
        if !(0..=MAX_SESSION_QUOTA_MB).contains(&self.session_quota_mb)
            || !(0..=MAX_GLOBAL_BUDGET_MB).contains(&self.global_budget_mb)
            || !(0..=MAX_GLOBAL_BUDGET_MB).contains(&self.min_free_space_mb)
        {
            return Err(TranscodingSettingsError::QuotaOutOfRange);
        }
        if !(0..=MAX_VIDEO_BITRATE_KBPS).contains(&self.max_video_bitrate_kbps)
            || !(MIN_AUDIO_BITRATE_KBPS..=MAX_AUDIO_BITRATE_KBPS).contains(&self.audio_bitrate_kbps)
        {
            return Err(TranscodingSettingsError::BitrateOutOfRange);
        }
        if self.max_output_height != 0
            && !(240..=MAX_OUTPUT_HEIGHT).contains(&self.max_output_height)
        {
            return Err(TranscodingSettingsError::OutputHeightOutOfRange);
        }
        Ok(self)
    }
}

/// Reads the stored policy, falling back to defaults for missing or unusable rows.
pub(crate) async fn read_transcoding_settings(database: &Database) -> TranscodingSettings {
    let stored = match database.transcoding_settings().await {
        Ok(stored) => stored,
        Err(_) => return TranscodingSettings::default(),
    };
    let Some(stored) = stored else {
        return TranscodingSettings::default();
    };
    match serde_json::from_str::<TranscodingSettings>(&stored)
        .ok()
        .and_then(|settings| settings.normalized().ok())
    {
        Some(settings) => settings,
        None => {
            tracing::warn!("stored transcoding settings are invalid; using defaults");
            TranscodingSettings::default()
        }
    }
}

pub(crate) async fn write_transcoding_settings(
    database: &Database,
    settings: &TranscodingSettings,
) -> Result<TranscodingSettings, TranscodingSettingsError> {
    let settings = settings.clone().normalized()?;
    let json = serde_json::to_string(&settings)
        .map_err(|_| TranscodingSettingsError::SessionsOutOfRange)?;
    database
        .set_transcoding_settings(&json)
        .await
        .map_err(|_| TranscodingSettingsError::SessionsOutOfRange)?;
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_HARDWARE_DEVICE, HARDWARE_ENCODER_NONE, TranscodingSettings,
        TranscodingSettingsError, read_transcoding_settings, write_transcoding_settings,
    };
    use crate::{config::Config, storage::Database};

    #[test]
    fn defaults_match_the_previous_built_in_limits() {
        let settings = TranscodingSettings::default();
        assert!(settings.enabled);
        assert_eq!(settings.hardware_encoder, HARDWARE_ENCODER_NONE);
        assert_eq!(settings.remux_sessions, 4);
        assert_eq!(settings.hardware_sessions, 2);
        assert_eq!(settings.software_sessions, 1);
        assert_eq!(settings.session_quota_mb, 2_048);
        assert_eq!(settings.global_budget_mb, 8_192);
        assert_eq!(settings.min_free_space_mb, 512);
        assert_eq!(settings.software_preset, "veryfast");
        assert_eq!(settings.audio_bitrate_kbps, 192);
        assert_eq!(settings.max_video_bitrate_kbps, 0);
        assert_eq!(settings.max_output_height, 0);
        assert_eq!(settings.hardware_device, DEFAULT_HARDWARE_DEVICE);
        assert!(!settings.hardware_decode);
    }

    #[test]
    fn hardware_device_is_trimmed_and_validated() {
        let normalized = TranscodingSettings {
            hardware_device: "  /dev/dri/renderD129 ".to_owned(),
            ..TranscodingSettings::default()
        }
        .normalized()
        .expect("a device path is accepted");
        assert_eq!(normalized.hardware_device, "/dev/dri/renderD129");

        let empty = TranscodingSettings {
            hardware_device: "   ".to_owned(),
            ..TranscodingSettings::default()
        }
        .normalized()
        .expect("empty falls back to the default device");
        assert_eq!(empty.hardware_device, DEFAULT_HARDWARE_DEVICE);

        let invalid = TranscodingSettings {
            hardware_device: "/dev/dri/renderD\u{7}128".to_owned(),
            ..TranscodingSettings::default()
        };
        assert_eq!(
            invalid.normalized().unwrap_err(),
            TranscodingSettingsError::DeviceUnusable
        );
    }

    #[test]
    fn missing_json_fields_keep_their_defaults() {
        let settings: TranscodingSettings =
            serde_json::from_str(r#"{"enabled":false}"#).expect("partial JSON must parse");
        assert!(!settings.enabled);
        assert_eq!(settings.software_sessions, 1);
        assert_eq!(settings.software_preset, "veryfast");
    }

    #[test]
    fn normalization_accepts_case_variants_and_rejects_unknown_values() {
        let settings = TranscodingSettings {
            hardware_encoder: " h264_Videotoolbox ".to_owned(),
            software_preset: "FASTER".to_owned(),
            ..TranscodingSettings::default()
        }
        .normalized()
        .expect("case variants are accepted");
        assert_eq!(settings.hardware_encoder, "h264_videotoolbox");
        assert_eq!(settings.software_preset, "faster");

        let auto = TranscodingSettings {
            hardware_encoder: "auto".to_owned(),
            ..TranscodingSettings::default()
        }
        .normalized()
        .expect("auto means no hardware encoder");
        assert_eq!(auto.hardware_encoder, HARDWARE_ENCODER_NONE);

        for (settings, expected) in [
            (
                TranscodingSettings {
                    hardware_encoder: "h264_unknown".to_owned(),
                    ..TranscodingSettings::default()
                },
                TranscodingSettingsError::UnknownEncoder,
            ),
            (
                TranscodingSettings {
                    software_preset: "placebo".to_owned(),
                    ..TranscodingSettings::default()
                },
                TranscodingSettingsError::UnknownPreset,
            ),
            (
                TranscodingSettings {
                    software_sessions: -1,
                    ..TranscodingSettings::default()
                },
                TranscodingSettingsError::SessionsOutOfRange,
            ),
            (
                TranscodingSettings {
                    global_budget_mb: 2_000_000,
                    ..TranscodingSettings::default()
                },
                TranscodingSettingsError::QuotaOutOfRange,
            ),
            (
                TranscodingSettings {
                    audio_bitrate_kbps: 1_024,
                    ..TranscodingSettings::default()
                },
                TranscodingSettingsError::BitrateOutOfRange,
            ),
            (
                TranscodingSettings {
                    max_output_height: 100,
                    ..TranscodingSettings::default()
                },
                TranscodingSettingsError::OutputHeightOutOfRange,
            ),
        ] {
            assert_eq!(settings.normalized().unwrap_err(), expected);
        }
    }

    #[tokio::test]
    async fn stored_settings_round_trip_and_corrupt_rows_fall_back_to_defaults()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        assert_eq!(
            read_transcoding_settings(&database).await,
            TranscodingSettings::default()
        );

        let saved = write_transcoding_settings(
            &database,
            &TranscodingSettings {
                enabled: false,
                software_sessions: 3,
                software_preset: "FAST".to_owned(),
                ..TranscodingSettings::default()
            },
        )
        .await?;
        assert_eq!(saved.software_preset, "fast");
        assert_eq!(read_transcoding_settings(&database).await, saved);

        sqlx::query("UPDATE server_settings SET value = '{not json' WHERE key = 'transcoding'")
            .execute(database.pool())
            .await?;
        assert_eq!(
            read_transcoding_settings(&database).await,
            TranscodingSettings::default()
        );
        Ok(())
    }
}
