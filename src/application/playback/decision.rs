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
}

impl fmt::Display for UnsupportedReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::StrmRequiresDirectPlay => "STRM_REQUIRES_DIRECT_PLAY",
            Self::BrowserCannotConsumeHls => "BROWSER_CANNOT_CONSUME_HLS",
            Self::NoCompatibleServerPlan => "NO_COMPATIBLE_SERVER_PLAN",
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

pub fn choose_plan(input: PlaybackDecisionInput) -> PlaybackPlan {
    if input.source_kind == PlaybackSourceKind::Strm {
        return if input.capabilities.direct_play {
            PlaybackPlan::Direct
        } else {
            PlaybackPlan::Unsupported {
                reason: UnsupportedReason::StrmRequiresDirectPlay,
            }
        };
    }

    if input.capabilities.direct_play {
        return PlaybackPlan::Direct;
    }
    if !input.capabilities.hls {
        return PlaybackPlan::Unsupported {
            reason: UnsupportedReason::BrowserCannotConsumeHls,
        };
    }
    if input.capabilities.video_copy_to_fmp4 && input.capabilities.audio_copy_to_fmp4 {
        return PlaybackPlan::ServerHls {
            tier: ServerTier::Remux,
        };
    }
    if input.capabilities.video_copy_to_fmp4 {
        return PlaybackPlan::ServerHls {
            tier: ServerTier::AudioTranscode,
        };
    }
    if input.capabilities.hardware_transcode {
        return PlaybackPlan::ServerHls {
            tier: ServerTier::HardwareTranscode,
        };
    }
    if input.capabilities.software_transcode {
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
        PlaybackCapabilities, PlaybackDecisionInput, PlaybackPlan, PlaybackSourceKind, ServerTier,
        UnsupportedReason, choose_plan,
    };

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
}
