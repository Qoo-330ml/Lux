use std::fmt;

/// The server-side processing level. Client-side decoders remain part of tier 0
/// because the server still returns the original media bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServerTier {
    Direct = 0,
    Remux = 1,
    AudioTranscode = 2,
    HardwareTranscode = 3,
    SoftwareTranscode = 4,
}

impl ServerTier {
    pub const fn number(self) -> u8 {
        self as u8
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaybackSourceKind {
    LocalFile,
    Strm,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsupportedReason {
    StrmRequiresDirectPlay,
    BrowserCannotConsumeHls,
    NoCompatibleServerPlan,
    /// An administrator turned server-side transcoding off, so only direct play is offered.
    ServerTranscodingDisabled,
}

impl fmt::Display for UnsupportedReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::StrmRequiresDirectPlay => "STRM_REQUIRES_DIRECT_PLAY",
            Self::BrowserCannotConsumeHls => "BROWSER_CANNOT_CONSUME_HLS",
            Self::NoCompatibleServerPlan => "NO_COMPATIBLE_SERVER_PLAN",
            Self::ServerTranscodingDisabled => "SERVER_TRANSCODING_DISABLED",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaybackPlan {
    Direct,
    ServerHls { tier: ServerTier },
    Unsupported { reason: UnsupportedReason },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlaybackCapabilities {
    /// The browser can play the original source through native video or a
    /// client-side fallback that still reads the original Range endpoint.
    pub direct_play: bool,
    pub hls: bool,
    pub video_copy_to_fmp4: bool,
    pub audio_copy_to_fmp4: bool,
    pub hardware_transcode: bool,
    pub software_transcode: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlaybackDecisionInput {
    pub source_kind: PlaybackSourceKind,
    pub capabilities: PlaybackCapabilities,
}

/// Lowest accepted ceiling for a user-selected output bitrate, in bits per second (100 kbit/s).
///
/// Below it no browser-decodable video fits, so such a request cannot be honoured.
pub const MIN_MAX_BITRATE_BPS: i64 = 100_000;

/// Highest accepted ceiling for a user-selected output bitrate, in bits per second (200 Mbit/s).
///
/// A larger value would silently behave as "no ceiling", so it is rejected instead.
pub const MAX_MAX_BITRATE_BPS: i64 = 200_000_000;

/// Whether `value` is an acceptable user-selected output ceiling in bits per second.
pub const fn valid_max_bitrate_bps(value: i64) -> bool {
    MIN_MAX_BITRATE_BPS <= value && value <= MAX_MAX_BITRATE_BPS
}

/// The output ceiling one playback request selected, together with the source it applies to.
///
/// A ceiling never changes what the browser can consume; it only rules out the plans whose output
/// would stay above it. A copied video track keeps the source bitrate, so only a re-encode can
/// honour a ceiling the source exceeds.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PlaybackBitrateLimit {
    /// Container bitrate measured for the source, when the probe recorded one.
    pub source_bitrate_bps: Option<i64>,
    /// The ceiling the caller selected, when any.
    pub max_bitrate_bps: Option<i64>,
}

impl PlaybackBitrateLimit {
    /// No ceiling: the decision is the plain lowest-cost plan.
    pub const NONE: Self = Self {
        source_bitrate_bps: None,
        max_bitrate_bps: None,
    };

    /// The source is known to be above the ceiling, so a copied video track would stay above it.
    ///
    /// An unknown source bitrate never forces a re-encode: the server cannot prove the ceiling is
    /// exceeded, and an unprobed file would otherwise be transcoded for no reason.
    pub const fn requires_video_transcode(&self) -> bool {
        match (self.max_bitrate_bps, self.source_bitrate_bps) {
            (Some(max), Some(source)) => source > max,
            _ => false,
        }
    }

    /// The video bitrate a re-encode has to stay at or below, when the ceiling forced one.
    pub fn target_video_bitrate_bps(&self) -> Option<i64> {
        if self.requires_video_transcode() {
            self.max_bitrate_bps
        } else {
            None
        }
    }
}

/// Lowest accepted resolution ceiling, in output pixels of height (144p).
pub const MIN_MAX_HEIGHT: i64 = 144;

/// Highest accepted resolution ceiling, in output pixels of height (2160p is the largest Web
/// ladder rung; anything above that is refused as malformed rather than treated as "no limit").
pub const MAX_MAX_HEIGHT: i64 = 4320;

/// Whether `value` is an acceptable user-selected output height in pixels.
pub const fn valid_max_height(value: i64) -> bool {
    MIN_MAX_HEIGHT <= value && value <= MAX_MAX_HEIGHT
}

/// Video bitrate the server derives from an output height when the caller only picked a
/// resolution.
///
/// Keeping the ladder in one table makes the derived value auditable and tunable: about
/// 1 Mbit/s up to 360p, the usual doubling through 480p and 720p, 8 Mbit/s for 1080p,
/// 12 Mbit/s for 1440p and 20 Mbit/s for anything above that. The result is still narrowed by
/// the source bitrate, the caller's explicit ceiling and the administrator cap.
pub const fn height_band_video_bitrate_bps(height: i64) -> i64 {
    match height {
        ..=360 => 1_000_000,
        ..=480 => 2_000_000,
        ..=720 => 4_000_000,
        ..=1080 => 8_000_000,
        ..=1440 => 12_000_000,
        _ => 20_000_000,
    }
}

/// The output resolution one playback request selected, together with the source it applies to.
///
/// A height ceiling never changes what the browser can consume; it only rules out plans whose
/// output would stay above it. Only a re-encode can shrink the picture, so a source known to be
/// taller than the ceiling is re-encoded.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PlaybackResolutionLimit {
    /// Video height measured for the source, when the probe recorded one.
    pub source_height: Option<i64>,
    /// The resolution ceiling the caller selected, when any.
    pub max_height: Option<i64>,
}

impl PlaybackResolutionLimit {
    /// No height ceiling: the decision is the plain lowest-cost plan.
    pub const NONE: Self = Self {
        source_height: None,
        max_height: None,
    };

    /// The source is known to be taller than the ceiling, so it has to be scaled down.
    ///
    /// An unknown source height never forces a re-encode: the server cannot prove the picture is
    /// too tall, and an unprobed file would otherwise be transcoded for no reason.
    pub const fn requires_scale(&self) -> bool {
        match (self.max_height, self.source_height) {
            (Some(max), Some(source)) => source > max,
            _ => false,
        }
    }

    /// The height the picture ends up at when only the caller's ceiling applies.
    pub const fn scaled_height(&self) -> Option<i64> {
        match (self.source_height, self.max_height) {
            (Some(source), Some(max)) => Some(if source < max { source } else { max }),
            (source, None) => source,
            (None, max) => max,
        }
    }

    /// The height the output actually has, once the administrator's global cap applies.
    ///
    /// The cap only narrows a re-encode: a copied track keeps the source picture, so it is
    /// ignored for Direct Play, Remux and audio transcode. `admin_cap` of `0` or less means the
    /// administrator configured no global height cap.
    pub fn output_height(&self, admin_cap: i64, video_re_encoded: bool) -> Option<i64> {
        let scaled = self.scaled_height();
        if !video_re_encoded || admin_cap <= 0 {
            return scaled;
        }
        match scaled {
            Some(scaled) => Some(scaled.min(admin_cap)),
            None => Some(admin_cap),
        }
    }
}

/// Smallest of the two ceilings; `None` for a side that is unknown never wins.
fn narrow(current: Option<i64>, candidate: Option<i64>) -> Option<i64> {
    match (current, candidate) {
        (Some(current), Some(candidate)) => Some(current.min(candidate)),
        (Some(current), None) => Some(current),
        (None, candidate) => candidate,
    }
}

/// The `-b:v` a re-encode is started with, or `None` when no limit asked for one.
///
/// The candidates are the caller's explicit ceiling, the bitrate derived from the target height,
/// and the source's own bitrate (an encode of a smaller picture never needs to spend more than
/// the source already carries). The smallest wins; the administrator cap is applied later, when
/// the encoder starts, because it applies even when the caller selected nothing.
pub fn reencode_video_bitrate_bps(
    bitrate: PlaybackBitrateLimit,
    resolution: PlaybackResolutionLimit,
) -> Option<i64> {
    let mut target = None;
    if bitrate.requires_video_transcode() {
        target = narrow(target, bitrate.max_bitrate_bps);
    }
    if resolution.requires_scale() {
        target = narrow(
            target,
            resolution
                .scaled_height()
                .map(height_band_video_bitrate_bps),
        );
        // A re-encode must not spend more than the source already carries; an unmeasured (or
        // zero) source bitrate simply adds no constraint.
        target = narrow(
            target,
            bitrate.source_bitrate_bps.filter(|value| *value > 0),
        );
        // An explicit ceiling still bounds the output even when the source already fits it.
        target = narrow(target, bitrate.max_bitrate_bps);
    }
    target
}

/// The plain decision for a request that selected no output ceiling.
pub fn choose_plan(input: PlaybackDecisionInput) -> PlaybackPlan {
    choose_plan_with_bitrate(input, PlaybackBitrateLimit::NONE)
}

/// The playback plan for one request, honouring only the caller's bitrate ceiling.
pub fn choose_plan_with_bitrate(
    input: PlaybackDecisionInput,
    limit: PlaybackBitrateLimit,
) -> PlaybackPlan {
    choose_plan_with_limits(input, limit, PlaybackResolutionLimit::NONE)
}

/// The playback plan for one request, honouring both of the caller's output limits.
///
/// A source already inside its ceilings keeps the plain lowest-cost decision. A source known to
/// be above either ceiling is re-encoded on the cheapest tier that can do so, because Direct
/// Play, Remux and audio transcode would hand the browser the original bitrate or picture.
pub fn choose_plan_with_limits(
    input: PlaybackDecisionInput,
    limit: PlaybackBitrateLimit,
    resolution: PlaybackResolutionLimit,
) -> PlaybackPlan {
    if input.source_kind == PlaybackSourceKind::Strm {
        // `.strm` stays Direct-only. The ceilings are ignored rather than rejected: the server
        // does not probe remote targets, so it usually cannot even measure what it would cap.
        return if input.capabilities.direct_play {
            PlaybackPlan::Direct
        } else {
            PlaybackPlan::Unsupported {
                reason: UnsupportedReason::StrmRequiresDirectPlay,
            }
        };
    }

    if limit.requires_video_transcode() || resolution.requires_scale() {
        return if !input.capabilities.hls {
            PlaybackPlan::Unsupported {
                reason: UnsupportedReason::BrowserCannotConsumeHls,
            }
        } else if input.capabilities.hardware_transcode {
            PlaybackPlan::ServerHls {
                tier: ServerTier::HardwareTranscode,
            }
        } else if input.capabilities.software_transcode {
            PlaybackPlan::ServerHls {
                tier: ServerTier::SoftwareTranscode,
            }
        } else {
            // Re-encoding is the only way to honour the ceiling and this server cannot do it.
            PlaybackPlan::Unsupported {
                reason: UnsupportedReason::NoCompatibleServerPlan,
            }
        };
    }

    lowest_cost_local_plan(input.capabilities)
}

/// The plain 0→4 decision for a local file, without any bitrate ceiling.
fn lowest_cost_local_plan(capabilities: PlaybackCapabilities) -> PlaybackPlan {
    if capabilities.direct_play {
        return PlaybackPlan::Direct;
    }
    if !capabilities.hls {
        return PlaybackPlan::Unsupported {
            reason: UnsupportedReason::BrowserCannotConsumeHls,
        };
    }
    if capabilities.video_copy_to_fmp4 && capabilities.audio_copy_to_fmp4 {
        return PlaybackPlan::ServerHls {
            tier: ServerTier::Remux,
        };
    }
    if capabilities.video_copy_to_fmp4 {
        return PlaybackPlan::ServerHls {
            tier: ServerTier::AudioTranscode,
        };
    }
    if capabilities.hardware_transcode {
        return PlaybackPlan::ServerHls {
            tier: ServerTier::HardwareTranscode,
        };
    }
    if capabilities.software_transcode {
        return PlaybackPlan::ServerHls {
            tier: ServerTier::SoftwareTranscode,
        };
    }
    PlaybackPlan::Unsupported {
        reason: UnsupportedReason::NoCompatibleServerPlan,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_MAX_BITRATE_BPS, MAX_MAX_HEIGHT, MIN_MAX_BITRATE_BPS, MIN_MAX_HEIGHT,
        PlaybackBitrateLimit, PlaybackCapabilities, PlaybackDecisionInput, PlaybackPlan,
        PlaybackResolutionLimit, PlaybackSourceKind, ServerTier, UnsupportedReason, choose_plan,
        choose_plan_with_bitrate, choose_plan_with_limits, height_band_video_bitrate_bps,
        reencode_video_bitrate_bps, valid_max_bitrate_bps, valid_max_height,
    };

    /// A local file the browser plays natively *and* the server could remux or transcode.
    const EVERYTHING_AVAILABLE: PlaybackCapabilities = PlaybackCapabilities {
        direct_play: true,
        hls: true,
        video_copy_to_fmp4: true,
        audio_copy_to_fmp4: true,
        hardware_transcode: true,
        software_transcode: true,
    };

    const fn ceiling(source_bitrate_bps: i64, max_bitrate_bps: i64) -> PlaybackBitrateLimit {
        PlaybackBitrateLimit {
            source_bitrate_bps: Some(source_bitrate_bps),
            max_bitrate_bps: Some(max_bitrate_bps),
        }
    }

    const fn local(capabilities: PlaybackCapabilities) -> PlaybackDecisionInput {
        PlaybackDecisionInput {
            source_kind: PlaybackSourceKind::LocalFile,
            capabilities,
        }
    }

    const fn strm(capabilities: PlaybackCapabilities) -> PlaybackDecisionInput {
        PlaybackDecisionInput {
            source_kind: PlaybackSourceKind::Strm,
            capabilities,
        }
    }

    #[test]
    fn direct_play_wins_over_every_server_fallback() {
        assert_eq!(
            choose_plan(local(PlaybackCapabilities {
                direct_play: true,
                hls: true,
                video_copy_to_fmp4: false,
                audio_copy_to_fmp4: false,
                hardware_transcode: true,
                software_transcode: true,
            })),
            PlaybackPlan::Direct
        );
    }

    #[test]
    fn local_media_uses_the_lowest_cost_hls_tier() {
        let base = PlaybackCapabilities {
            direct_play: false,
            hls: true,
            video_copy_to_fmp4: true,
            audio_copy_to_fmp4: true,
            hardware_transcode: true,
            software_transcode: true,
        };
        assert_eq!(
            choose_plan(local(base)),
            PlaybackPlan::ServerHls {
                tier: ServerTier::Remux
            }
        );
        assert_eq!(
            choose_plan(local(PlaybackCapabilities {
                audio_copy_to_fmp4: false,
                ..base
            })),
            PlaybackPlan::ServerHls {
                tier: ServerTier::AudioTranscode
            }
        );
        assert_eq!(
            choose_plan(local(PlaybackCapabilities {
                video_copy_to_fmp4: false,
                audio_copy_to_fmp4: false,
                ..base
            })),
            PlaybackPlan::ServerHls {
                tier: ServerTier::HardwareTranscode
            }
        );
    }

    #[test]
    fn software_transcode_is_only_used_when_hardware_is_unavailable() {
        assert_eq!(
            choose_plan(local(PlaybackCapabilities {
                direct_play: false,
                hls: true,
                video_copy_to_fmp4: false,
                audio_copy_to_fmp4: false,
                hardware_transcode: false,
                software_transcode: true,
            })),
            PlaybackPlan::ServerHls {
                tier: ServerTier::SoftwareTranscode
            }
        );
    }

    #[test]
    fn strm_never_falls_back_to_hls_or_transcode() {
        let capabilities = PlaybackCapabilities {
            direct_play: false,
            hls: true,
            video_copy_to_fmp4: true,
            audio_copy_to_fmp4: true,
            hardware_transcode: true,
            software_transcode: true,
        };
        assert_eq!(
            choose_plan(strm(capabilities)),
            PlaybackPlan::Unsupported {
                reason: UnsupportedReason::StrmRequiresDirectPlay
            }
        );
    }

    #[test]
    fn hls_unavailable_is_reported_before_server_processing() {
        assert_eq!(
            choose_plan(local(PlaybackCapabilities {
                direct_play: false,
                hls: false,
                video_copy_to_fmp4: true,
                audio_copy_to_fmp4: true,
                hardware_transcode: true,
                software_transcode: true,
            })),
            PlaybackPlan::Unsupported {
                reason: UnsupportedReason::BrowserCannotConsumeHls
            }
        );
    }

    #[test]
    fn a_ceiling_the_source_already_meets_changes_nothing() {
        let limit = ceiling(2_000_000, 4_000_000);
        assert_eq!(
            choose_plan_with_bitrate(local(EVERYTHING_AVAILABLE), limit),
            PlaybackPlan::Direct
        );
        assert_eq!(
            choose_plan_with_bitrate(
                local(PlaybackCapabilities {
                    direct_play: false,
                    ..EVERYTHING_AVAILABLE
                }),
                limit,
            ),
            PlaybackPlan::ServerHls {
                tier: ServerTier::Remux
            },
            "a source inside its ceiling must still remux instead of transcoding"
        );
        assert_eq!(
            choose_plan_with_bitrate(local(EVERYTHING_AVAILABLE), ceiling(4_000_000, 4_000_000)),
            PlaybackPlan::Direct,
            "the ceiling includes the source bitrate itself"
        );
        assert_eq!(
            limit.target_video_bitrate_bps(),
            None,
            "nothing is re-encoded, so there is no target bitrate"
        );
    }

    #[test]
    fn an_unrequested_or_unmeasurable_ceiling_changes_nothing() {
        let direct = choose_plan(local(EVERYTHING_AVAILABLE));
        assert_eq!(
            choose_plan_with_bitrate(local(EVERYTHING_AVAILABLE), PlaybackBitrateLimit::NONE),
            direct
        );
        assert_eq!(
            choose_plan_with_bitrate(
                local(EVERYTHING_AVAILABLE),
                PlaybackBitrateLimit {
                    source_bitrate_bps: Some(20_000_000),
                    max_bitrate_bps: None,
                },
            ),
            direct,
            "a request without a ceiling keeps the plain decision"
        );
        assert_eq!(
            choose_plan_with_bitrate(
                local(EVERYTHING_AVAILABLE),
                PlaybackBitrateLimit {
                    source_bitrate_bps: None,
                    max_bitrate_bps: Some(4_000_000),
                },
            ),
            direct,
            "an unprobed source cannot be proven to exceed the ceiling, so Direct Play stays"
        );
    }

    #[test]
    fn direct_play_is_upgraded_when_the_source_exceeds_the_ceiling() {
        let limit = ceiling(20_000_000, 4_000_000);
        assert!(limit.requires_video_transcode());
        assert_eq!(limit.target_video_bitrate_bps(), Some(4_000_000));
        assert_eq!(
            choose_plan_with_bitrate(local(EVERYTHING_AVAILABLE), limit),
            PlaybackPlan::ServerHls {
                tier: ServerTier::HardwareTranscode
            },
            "Direct Play would hand the browser the original bitrate"
        );
    }

    #[test]
    fn a_copy_tier_is_upgraded_to_a_re_encode_above_the_ceiling() {
        let limit = ceiling(20_000_000, 4_000_000);
        let remux_only = PlaybackCapabilities {
            direct_play: false,
            software_transcode: false,
            ..EVERYTHING_AVAILABLE
        };
        assert_eq!(
            choose_plan_with_bitrate(local(remux_only), limit),
            PlaybackPlan::ServerHls {
                tier: ServerTier::HardwareTranscode
            },
            "Remux copies the video track, so it stays above the ceiling"
        );
        let software_only = PlaybackCapabilities {
            direct_play: false,
            hardware_transcode: false,
            ..EVERYTHING_AVAILABLE
        };
        assert_eq!(
            choose_plan_with_bitrate(local(software_only), limit),
            PlaybackPlan::ServerHls {
                tier: ServerTier::SoftwareTranscode
            }
        );
    }

    #[test]
    fn a_ceiling_without_a_usable_re_encode_is_unsupported() {
        assert_eq!(
            choose_plan_with_bitrate(
                local(PlaybackCapabilities {
                    direct_play: true,
                    hardware_transcode: false,
                    software_transcode: false,
                    ..EVERYTHING_AVAILABLE
                }),
                ceiling(20_000_000, 4_000_000),
            ),
            PlaybackPlan::Unsupported {
                reason: UnsupportedReason::NoCompatibleServerPlan
            },
            "Direct Play cannot honour the ceiling on a server that cannot re-encode"
        );
        assert_eq!(
            choose_plan_with_bitrate(
                local(PlaybackCapabilities {
                    hls: false,
                    ..EVERYTHING_AVAILABLE
                }),
                ceiling(20_000_000, 4_000_000),
            ),
            PlaybackPlan::Unsupported {
                reason: UnsupportedReason::BrowserCannotConsumeHls
            }
        );
    }

    #[test]
    fn strm_ignores_the_ceiling_and_keeps_its_direct_only_rule() {
        let limit = ceiling(20_000_000, 4_000_000);
        assert_eq!(
            choose_plan_with_bitrate(strm(EVERYTHING_AVAILABLE), limit),
            PlaybackPlan::Direct,
            "a .strm ceiling is ignored: the target is never transcoded"
        );
        assert_eq!(
            choose_plan_with_bitrate(
                strm(PlaybackCapabilities {
                    direct_play: false,
                    ..EVERYTHING_AVAILABLE
                }),
                limit,
            ),
            PlaybackPlan::Unsupported {
                reason: UnsupportedReason::StrmRequiresDirectPlay
            }
        );
    }

    #[test]
    fn accepted_ceilings_are_bounded() {
        assert!(valid_max_bitrate_bps(MIN_MAX_BITRATE_BPS));
        assert!(valid_max_bitrate_bps(MAX_MAX_BITRATE_BPS));
        assert!(valid_max_bitrate_bps(4_000_000));
        assert!(!valid_max_bitrate_bps(MIN_MAX_BITRATE_BPS - 1));
        assert!(!valid_max_bitrate_bps(MAX_MAX_BITRATE_BPS + 1));
        assert!(!valid_max_bitrate_bps(0));
        assert!(!valid_max_bitrate_bps(-1));
    }

    const fn height(source_height: i64, max_height: i64) -> PlaybackResolutionLimit {
        PlaybackResolutionLimit {
            source_height: Some(source_height),
            max_height: Some(max_height),
        }
    }

    #[test]
    fn accepted_heights_are_bounded() {
        assert!(valid_max_height(MIN_MAX_HEIGHT));
        assert!(valid_max_height(MAX_MAX_HEIGHT));
        assert!(valid_max_height(1_080));
        assert!(!valid_max_height(MIN_MAX_HEIGHT - 1));
        assert!(!valid_max_height(MAX_MAX_HEIGHT + 1));
        assert!(!valid_max_height(0));
        assert!(!valid_max_height(-1));
    }

    #[test]
    fn the_height_bands_derive_one_bitrate_per_ladder_rung() {
        assert_eq!(height_band_video_bitrate_bps(144), 1_000_000);
        assert_eq!(height_band_video_bitrate_bps(360), 1_000_000);
        assert_eq!(height_band_video_bitrate_bps(361), 2_000_000);
        assert_eq!(height_band_video_bitrate_bps(480), 2_000_000);
        assert_eq!(height_band_video_bitrate_bps(481), 4_000_000);
        assert_eq!(height_band_video_bitrate_bps(720), 4_000_000);
        assert_eq!(height_band_video_bitrate_bps(721), 8_000_000);
        assert_eq!(height_band_video_bitrate_bps(1_080), 8_000_000);
        assert_eq!(height_band_video_bitrate_bps(1_440), 12_000_000);
        assert_eq!(height_band_video_bitrate_bps(1_441), 20_000_000);
        assert_eq!(height_band_video_bitrate_bps(4_320), 20_000_000);
    }

    #[test]
    fn only_a_source_known_to_be_taller_than_the_ceiling_is_scaled() {
        assert!(height(2_160, 1_080).requires_scale());
        assert!(!height(1_080, 1_080).requires_scale());
        assert!(!height(720, 1_080).requires_scale());
        assert!(
            !PlaybackResolutionLimit {
                source_height: None,
                max_height: Some(1_080),
            }
            .requires_scale(),
            "an unprobed source cannot be proven to be too tall"
        );
        assert!(
            !PlaybackResolutionLimit {
                source_height: Some(2_160),
                max_height: None,
            }
            .requires_scale(),
            "without a ceiling nothing is scaled"
        );
    }

    #[test]
    fn the_reported_output_height_is_the_smallest_known_ceiling() {
        assert_eq!(height(2_160, 1_080).output_height(0, true), Some(1_080));
        assert_eq!(
            height(2_160, 1_080).output_height(720, true),
            Some(720),
            "the administrator cap narrows the caller's ceiling"
        );
        assert_eq!(
            height(2_160, 1_080).output_height(720, false),
            Some(1_080),
            "a copied track keeps the source picture, so the cap does not apply"
        );
        assert_eq!(
            height(720, 1_080).output_height(0, false),
            Some(720),
            "nothing is scaled, so the source height is the output height"
        );
        assert_eq!(
            PlaybackResolutionLimit {
                source_height: None,
                max_height: Some(1_080),
            }
            .output_height(0, true),
            Some(1_080)
        );
        assert_eq!(
            PlaybackResolutionLimit::NONE.output_height(1_080, true),
            Some(1_080),
            "an administrator cap alone describes the output height"
        );
        assert_eq!(PlaybackResolutionLimit::NONE.output_height(0, true), None);
    }

    #[test]
    fn a_height_ceiling_the_source_already_meets_changes_nothing() {
        let resolution = height(720, 1_080);
        assert!(!resolution.requires_scale());
        assert_eq!(
            choose_plan_with_limits(
                local(EVERYTHING_AVAILABLE),
                PlaybackBitrateLimit::NONE,
                resolution
            ),
            PlaybackPlan::Direct
        );
        assert_eq!(
            choose_plan_with_limits(
                local(PlaybackCapabilities {
                    direct_play: false,
                    ..EVERYTHING_AVAILABLE
                }),
                PlaybackBitrateLimit::NONE,
                resolution
            ),
            PlaybackPlan::ServerHls {
                tier: ServerTier::Remux
            },
            "a source inside its ceiling must still remux instead of transcoding"
        );
        assert_eq!(
            reencode_video_bitrate_bps(PlaybackBitrateLimit::NONE, resolution),
            None,
            "nothing is re-encoded, so there is no target bitrate"
        );
    }

    #[test]
    fn a_source_taller_than_the_ceiling_is_re_encoded_on_the_cheapest_tier() {
        let resolution = height(2_160, 1_080);
        assert_eq!(
            choose_plan_with_limits(
                local(EVERYTHING_AVAILABLE),
                PlaybackBitrateLimit::NONE,
                resolution
            ),
            PlaybackPlan::ServerHls {
                tier: ServerTier::HardwareTranscode
            },
            "Direct Play would hand the browser the original picture"
        );
        let software_only = PlaybackCapabilities {
            hardware_transcode: false,
            ..EVERYTHING_AVAILABLE
        };
        assert_eq!(
            choose_plan_with_limits(local(software_only), PlaybackBitrateLimit::NONE, resolution),
            PlaybackPlan::ServerHls {
                tier: ServerTier::SoftwareTranscode
            }
        );
        assert_eq!(
            choose_plan_with_limits(
                local(PlaybackCapabilities {
                    hls: false,
                    ..EVERYTHING_AVAILABLE
                }),
                PlaybackBitrateLimit::NONE,
                resolution,
            ),
            PlaybackPlan::Unsupported {
                reason: UnsupportedReason::BrowserCannotConsumeHls
            }
        );
        assert_eq!(
            choose_plan_with_limits(
                local(PlaybackCapabilities {
                    hardware_transcode: false,
                    software_transcode: false,
                    ..EVERYTHING_AVAILABLE
                }),
                PlaybackBitrateLimit::NONE,
                resolution,
            ),
            PlaybackPlan::Unsupported {
                reason: UnsupportedReason::NoCompatibleServerPlan
            }
        );
    }

    #[test]
    fn strm_ignores_the_height_ceiling_and_keeps_its_direct_only_rule() {
        let resolution = height(2_160, 1_080);
        assert_eq!(
            choose_plan_with_limits(
                strm(EVERYTHING_AVAILABLE),
                PlaybackBitrateLimit::NONE,
                resolution
            ),
            PlaybackPlan::Direct,
            "a .strm height ceiling is ignored: the target is never transcoded"
        );
        assert_eq!(
            choose_plan_with_limits(
                strm(PlaybackCapabilities {
                    direct_play: false,
                    ..EVERYTHING_AVAILABLE
                }),
                PlaybackBitrateLimit::NONE,
                resolution,
            ),
            PlaybackPlan::Unsupported {
                reason: UnsupportedReason::StrmRequiresDirectPlay
            }
        );
    }

    #[test]
    fn the_strictest_of_both_ceilings_wins() {
        let resolution = height(2_160, 1_080);
        let bitrate = ceiling(20_000_000, 4_000_000);
        assert_eq!(
            choose_plan_with_limits(local(EVERYTHING_AVAILABLE), bitrate, resolution),
            PlaybackPlan::ServerHls {
                tier: ServerTier::HardwareTranscode
            }
        );
        assert_eq!(
            reencode_video_bitrate_bps(bitrate, resolution),
            Some(4_000_000),
            "the explicit ceiling is below the height ladder"
        );
        let high_bitrate_ceiling = ceiling(20_000_000, 50_000_000);
        assert_eq!(
            reencode_video_bitrate_bps(high_bitrate_ceiling, resolution),
            Some(8_000_000),
            "a generous bitrate ceiling leaves the height ladder in charge"
        );
        // A 2160p source scaled to 1080p derives 8 Mbit/s, which an explicit 12 Mbit/s ceiling
        // leaves untouched.
        let generous_ceiling = PlaybackBitrateLimit {
            source_bitrate_bps: None,
            max_bitrate_bps: Some(12_000_000),
        };
        assert_eq!(
            reencode_video_bitrate_bps(generous_ceiling, resolution),
            Some(8_000_000),
            "the derived bitrate is the stricter ceiling"
        );
    }

    #[test]
    fn a_resolution_limited_encode_never_exceeds_the_source_bitrate() {
        let resolution = height(2_160, 1_080);
        let measured = PlaybackBitrateLimit {
            source_bitrate_bps: Some(3_000_000),
            max_bitrate_bps: None,
        };
        assert_eq!(
            reencode_video_bitrate_bps(measured, resolution),
            Some(3_000_000),
            "no re-encode needs more than the source already carries"
        );
        let unknown = PlaybackBitrateLimit {
            source_bitrate_bps: None,
            max_bitrate_bps: None,
        };
        assert_eq!(
            reencode_video_bitrate_bps(unknown, resolution),
            Some(8_000_000),
            "without a measured source the height ladder decides"
        );
        let zero = PlaybackBitrateLimit {
            source_bitrate_bps: Some(0),
            max_bitrate_bps: None,
        };
        assert_eq!(
            reencode_video_bitrate_bps(zero, resolution),
            Some(8_000_000),
            "a zero measurement is unknown, not a ceiling"
        );
    }

    #[test]
    fn an_explicit_ceiling_still_bounds_a_height_limited_encode() {
        let resolution = height(2_160, 1_080);
        let ceiling = PlaybackBitrateLimit {
            // The source already fits its ceiling, so the bitrate alone would not re-encode.
            source_bitrate_bps: Some(6_000_000),
            max_bitrate_bps: Some(6_000_000),
        };
        assert!(!ceiling.requires_video_transcode());
        assert_eq!(
            reencode_video_bitrate_bps(ceiling, resolution),
            Some(6_000_000),
            "the height ladder (8 Mbit/s) is narrowed by the caller's ceiling"
        );
    }
}
