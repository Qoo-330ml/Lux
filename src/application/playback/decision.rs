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

/// The plain decision for a request that selected no output ceiling.
pub fn choose_plan(input: PlaybackDecisionInput) -> PlaybackPlan {
    choose_plan_with_bitrate(input, PlaybackBitrateLimit::NONE)
}

/// The playback plan for one request, honouring the caller's output ceiling.
///
/// A source already inside its ceiling keeps the plain lowest-cost decision. A source known to be
/// above it is re-encoded on the cheapest tier that can do so, because Direct Play and Remux would
/// hand the browser the original — too high — bitrate.
pub fn choose_plan_with_bitrate(
    input: PlaybackDecisionInput,
    limit: PlaybackBitrateLimit,
) -> PlaybackPlan {
    if input.source_kind == PlaybackSourceKind::Strm {
        // `.strm` stays Direct-only. The ceiling is ignored rather than rejected: the server does
        // not probe remote targets, so it usually cannot even measure what it would be capping.
        return if input.capabilities.direct_play {
            PlaybackPlan::Direct
        } else {
            PlaybackPlan::Unsupported {
                reason: UnsupportedReason::StrmRequiresDirectPlay,
            }
        };
    }

    if limit.requires_video_transcode() {
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
        MAX_MAX_BITRATE_BPS, MIN_MAX_BITRATE_BPS, PlaybackBitrateLimit, PlaybackCapabilities,
        PlaybackDecisionInput, PlaybackPlan, PlaybackSourceKind, ServerTier, UnsupportedReason,
        choose_plan, choose_plan_with_bitrate, valid_max_bitrate_bps,
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
}
