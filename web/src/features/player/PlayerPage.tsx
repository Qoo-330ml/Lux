import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent,
  type PointerEvent as ReactPointerEvent,
  type SyntheticEvent,
} from "react";
import { useNavigate, useParams, useSearchParams } from "react-router-dom";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type {
  MediaItem,
  MediaSource,
  PlaybackEventState,
  WebPlaybackCapabilities,
} from "../../lib/api/types";
import { imageUrl, mediaTitle } from "../home/media";
import {
  NativeVideoEngine,
  PLAYBACK_PERFORMANCE_EVENT,
  type PlaybackEngine,
  type PlaybackPerformance,
} from "./playback-engine";
import { HlsVideoEngine, canUseHls } from "./hls-playback-engine";
import { shouldUseClientHevc, shouldUseClientMkv } from "./playback-selection";
import { LegacyPlaybackEngineAdapter } from "./core/legacy-engine-adapter";
import { LuxPlayerRuntime } from "./core/player-runtime";
import { PlayerControls } from "./components/player-controls";
import { PlayerErrorState, PlayerLoadingState } from "./components/player-state";
import { PlayerSettingsPanel } from "./components/player-settings-panel";
import { PlayerTopBar, type PlayerSourceOption } from "./components/player-top-bar";
import { PlayerVideoSurface } from "./components/player-video-surface";

const TICKS_PER_SECOND = 10_000_000;
const PROGRESS_REPORT_INTERVAL_MS = 10_000;
const AUTO_HIDE_DELAY_MS = 3_000;
const PLAYBACK_SPEEDS = [0.5, 0.75, 1.0, 1.25, 1.5, 2.0];

const HEVC_RUNTIME_ASSETS = {
  workerUrl: "/hevc/transcode-worker.js",
  wasmUrl: "/hevc/hevc-decode-module.js",
  wasmBinaryUrl: "/hevc/hevc-decode.wasm",
};

function getMediaBadge(source?: MediaSource | null) {
  if (!source) return null;
  const badges: string[] = [];
  if (source.qualityLabel) badges.push(source.qualityLabel);
  const videoStream = source.streams?.find((s) => s.type === "VIDEO");
  if (videoStream?.codec) {
    badges.push(videoStream.codec.toUpperCase());
  }
  if (source.sourceKind === "STRM_URL") {
    badges.push("STRM");
  } else if (source.container) {
    badges.push(source.container.toUpperCase());
  }
  return badges.length > 0 ? badges.slice(0, 3).join(" • ") : null;
}

function getSubtitleInfo(media?: MediaItem | null) {
  if (!media) return null;
  if (media.itemType === "EPISODE") {
    const season = media.parentIndexNumber != null ? `第 ${media.parentIndexNumber} 季 ` : "";
    const episode = media.indexNumber != null ? `第 ${media.indexNumber} 集` : "";
    return `${season}${episode}`.trim() || null;
  }
  if (media.productionYear) {
    return String(media.productionYear);
  }
  return null;
}

export function webPlaybackCapabilities(
  source: MediaSource | undefined,
  attempt: number,
  videoOverride?: HTMLVideoElement | null,
): WebPlaybackCapabilities {
  const streams = source?.streams ?? [];
  const video = videoOverride ?? (typeof document === "undefined" ? null : document.createElement("video"));
  const videoCodec = (streams.find((stream) => stream.type?.toUpperCase() === "VIDEO")?.codec ?? "").toLowerCase();
  const audioCodec = (streams.find((stream) => stream.type?.toUpperCase() === "AUDIO")?.codec ?? "").toLowerCase();
  const videoCopyToFmp4 = supportsMp4Codec(video, "video", videoCodec);
  const audioCopyToFmp4 = !audioCodec || supportsMp4Codec(video, "audio", audioCodec);
  return {
    directPlay: attempt === 0,
    hls: canUseHls(video),
    videoCopyToFmp4,
    audioCopyToFmp4,
    hardwareTranscode: false,
    softwareTranscode: true,
  };
}

function supportsMp4Codec(
  video: HTMLVideoElement | null,
  kind: "audio" | "video",
  codec: string,
): boolean {
  if (!video || !codec) return false;
  const candidates = codecCandidates(kind, codec);
  return candidates.some((candidate) => {
    const mime = `${kind}/mp4; codecs="${candidate}"`;
    if (video.canPlayType(mime) !== "") return true;
    return typeof MediaSource !== "undefined"
      && typeof MediaSource.isTypeSupported === "function"
      && MediaSource.isTypeSupported(mime);
  });
}

function codecCandidates(kind: "audio" | "video", codec: string): string[] {
  const normalized = codec.toLowerCase();
  if (kind === "audio") {
    if (/^(aac|mp4a)(\.|$)/.test(normalized)) return [codec, "mp4a.40.2", "mp4a"];
    if (normalized === "ac3" || normalized === "ac-3") return [codec, "ac-3"];
    if (normalized === "eac3" || normalized === "ec-3") return [codec, "ec-3"];
    return [codec];
  }
  if (/^(h264|avc|avc1)(\.|$)/.test(normalized)) return [codec, "avc1"];
  if (/^(hevc|h265|hvc1|hev1)(\.|$)/.test(normalized)) return [codec, "hvc1"];
  if (/^(vp9|vp09)(\.|$)/.test(normalized)) return [codec, "vp09"];
  if (/^(av1|av01)(\.|$)/.test(normalized)) return [codec, "av01"];
  return [codec];
}

export function PlayerPage() {
  const { itemId = "" } = useParams();
  const [searchParams, setSearchParams] = useSearchParams();
  const navigate = useNavigate();
  const queryClient = useQueryClient();

  const [playing, setPlaying] = useState(false);
  const [playbackAttempt, setPlaybackAttempt] = useState(0);
  const [directProxyFallbackRequested, setDirectProxyFallbackRequested] = useState(false);
  const [failedStreamUrl, setFailedStreamUrl] = useState<string | null>(null);
  const [playbackError, setPlaybackError] = useState<string | null>(null);
  const [fallbackLoading, setFallbackLoading] = useState(false);
  const [fallbackSpeedX, setFallbackSpeedX] = useState<number | null>(null);

  // Playback control states
  const [currentTime, setCurrentTime] = useState(0);
  const [duration, setDuration] = useState(0);
  const [bufferedEnd, setBufferedEnd] = useState(0);
  const [volume, setVolume] = useState(1);
  const [isMuted, setIsMuted] = useState(false);
  const [playbackRate, setPlaybackRate] = useState(1.0);
  const [isFullscreen, setIsFullscreen] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [isRemainingTime, setIsRemainingTime] = useState(false);
  const [controlsVisible, setControlsVisible] = useState(true);
  const [centerSplash, setCenterSplash] = useState<"play" | "pause" | null>(null);
  const [hoverTime, setHoverTime] = useState<number | null>(null);
  const [hoverPercent, setHoverPercent] = useState<number | null>(null);

  const requestedSourceId = searchParams.get("sourceId");
  const item = useQuery({
    queryKey: queryKeys.item(itemId),
    queryFn: () => api.item(itemId),
    enabled: Boolean(itemId),
  });
  const playback = useQuery({
    queryKey: queryKeys.playback(itemId),
    queryFn: () => api.playback(itemId),
    enabled: Boolean(itemId),
  });

  const media = item.data;
  const source =
    media?.mediaSources?.find((entry) => entry.id === requestedSourceId) ??
    media?.mediaSources?.find((entry) => entry.isDefault) ??
    media?.mediaSources?.[0];
  const capabilities = webPlaybackCapabilities(source, playbackAttempt);
  const webPlaybackSession = useQuery({
    queryKey: queryKeys.webPlaybackSession(itemId, source?.id ?? "", playbackAttempt),
    queryFn: () => api.createWebPlaybackSession(itemId, source?.id ?? "", capabilities),
    enabled: Boolean(itemId && source?.id),
    retry: false,
    staleTime: Number.POSITIVE_INFINITY,
  });
  const playbackPlan = webPlaybackSession.data?.plan;
  const directProxyUrl = playbackPlan?.type === "DIRECT" ? playbackPlan.proxyUrl : undefined;
  const streamUrl = playbackPlan?.type === "DIRECT"
    ? (directProxyFallbackRequested ? playbackPlan.url : directProxyUrl ?? playbackPlan.url)
    : playbackPlan?.type === "SERVER_HLS"
      ? playbackPlan.manifestUrl
      : "";
  const poster = media ? imageUrl(media, "fanart") ?? imageUrl(media) : null;

  const playerContainerRef = useRef<HTMLDivElement>(null);
  const videoRef = useRef<HTMLVideoElement>(null);
  const lastVideoRef = useRef<HTMLVideoElement | null>(null);
  const engineRef = useRef<PlaybackEngine | null>(null);
  const runtimeRef = useRef<LuxPlayerRuntime | null>(null);
  const lastProgressReportRef = useRef(0);
  const hasStartedRef = useRef(false);
  const hasRestoredPositionRef = useRef(false);
  const hideControlsTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const splashTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const progressBarRef = useRef<HTMLDivElement>(null);
  const isDraggingScrubberRef = useRef(false);
  const playbackSessionIdRef = useRef<string | null>(null);
  const playbackSequenceRef = useRef(0);
  const fallbackRequestedRef = useRef(false);

  const setVideoRef = useCallback((video: HTMLVideoElement | null) => {
    if (!video) {
      const runtime = runtimeRef.current;
      runtimeRef.current = null;
      if (runtime?.element) {
        runtime.destroy();
      } else {
        engineRef.current?.destroy();
      }
      engineRef.current = null;
      videoRef.current = null;
      return;
    }
    videoRef.current = video;
    lastVideoRef.current = video;
    engineRef.current = new NativeVideoEngine(video);
  }, []);

  const reportPlayback = useCallback(
    (
      state: PlaybackEventState,
      force = false,
      keepalive = false,
      videoOverride?: HTMLVideoElement | null,
    ) => {
      const video = videoOverride ?? videoRef.current;
      if (!video || (state === "STOPPED" && !hasStartedRef.current)) return undefined;
      const now = Date.now();
      if (!force && now - lastProgressReportRef.current < PROGRESS_REPORT_INTERVAL_MS) return undefined;
      const positionTicks = Math.max(
        0,
        Math.round(
          (Number.isFinite(video.currentTime) ? video.currentTime : 0) * TICKS_PER_SECOND,
        ),
      );
      const durationTicks =
        Number.isFinite(video.duration) && video.duration >= 0
          ? Math.round(video.duration * TICKS_PER_SECOND)
          : null;
      lastProgressReportRef.current = now;
      const sessionId = playbackSessionIdRef.current;
      if (!sessionId) return undefined;
      const sequence = ++playbackSequenceRef.current;
      const request = api.webPlaybackEvent(
        sessionId,
        {
          eventId: `web-${sessionId}-${sequence}-${now}`,
          sequence,
          state,
          positionTicks,
          durationTicks,
        },
        keepalive,
      );
      if (state === "STOPPED") {
        return request
          .then(() => queryClient.invalidateQueries({ queryKey: queryKeys.home }))
          .catch(() => undefined);
      } else {
        void request.catch(() => undefined);
        return request;
      }
    },
    [queryClient],
  );

  const stopActiveSession = useCallback((keepalive = false) => {
    const sessionId = playbackSessionIdRef.current;
    if (!sessionId) return Promise.resolve();
    return api.stopWebPlaybackSession(sessionId, keepalive).catch(() => undefined);
  }, []);

  const requestServerFallback = useCallback((reason?: string) => {
    if (
      playbackPlan?.type === "DIRECT"
      && directProxyUrl
      && !directProxyFallbackRequested
    ) {
      setDirectProxyFallbackRequested(true);
      setFailedStreamUrl(null);
      setPlaybackError("外部代理播放失败，正在尝试 Lux 直放…");
      return;
    }
    if (
      playbackAttempt !== 0 ||
      playbackPlan?.type !== "DIRECT" ||
      fallbackRequestedRef.current
    ) {
      setFailedStreamUrl(streamUrl || null);
      if (reason) setPlaybackError(`客户端播放失败：${reason}`);
      return;
    }
    fallbackRequestedRef.current = true;
    const sessionId = playbackSessionIdRef.current;
    if (sessionId) void api.stopWebPlaybackSession(sessionId).catch(() => undefined);
    setPlaybackError("浏览器无法播放这个媒体源，正在准备兼容的服务端播放…");
    setFailedStreamUrl(null);
    setPlaybackAttempt(1);
  }, [directProxyFallbackRequested, directProxyUrl, playbackAttempt, playbackPlan?.type, streamUrl]);

  useEffect(() => {
    lastProgressReportRef.current = 0;
    hasStartedRef.current = false;
    hasRestoredPositionRef.current = false;
    fallbackRequestedRef.current = false;
    playbackSessionIdRef.current = null;
    playbackSequenceRef.current = 0;
    setPlaybackAttempt(0);
    setDirectProxyFallbackRequested(false);
    setFailedStreamUrl(null);
    setPlaybackError(null);
    setFallbackLoading(false);
    setFallbackSpeedX(null);
    setCurrentTime(0);
    setDuration(0);
    setBufferedEnd(0);
  }, [itemId, requestedSourceId]);

  useEffect(() => {
    playbackSessionIdRef.current = webPlaybackSession.data?.sessionId ?? null;
    playbackSequenceRef.current = 0;
  }, [webPlaybackSession.data?.sessionId]);

  useEffect(() => {
    const sessionId = webPlaybackSession.data?.sessionId;
    if (!sessionId) return;
    const heartbeat = window.setInterval(() => {
      void api.webPlaybackHeartbeat(sessionId).catch(() => undefined);
    }, 60_000);
    return () => window.clearInterval(heartbeat);
  }, [webPlaybackSession.data?.sessionId]);

  useEffect(() => {
    const handlePageHide = () => {
      void Promise.resolve(reportPlayback("STOPPED", true, true)).finally(() => {
        void stopActiveSession(true);
      });
    };
    window.addEventListener("pagehide", handlePageHide);
    return () => {
      window.removeEventListener("pagehide", handlePageHide);
      void Promise.resolve(reportPlayback("STOPPED", true, false, lastVideoRef.current)).finally(() => {
        void stopActiveSession();
      });
    };
  }, [reportPlayback, stopActiveSession]);

  const restorePlaybackPosition = useCallback(() => {
    if (hasRestoredPositionRef.current) return;
    const video = videoRef.current;
    if (!video || !playback.data) return;
    if (video.readyState < 1 && !Number.isFinite(video.duration)) return;
    hasRestoredPositionRef.current = true;
    const resumeTicks = playback.data.positionTicks ?? 0;
    if (playback.data.isPlayed || resumeTicks <= 0) return;
    const resumeSeconds = resumeTicks / TICKS_PER_SECOND;
    if (!Number.isFinite(video.duration) || resumeSeconds < video.duration) {
      video.currentTime = resumeSeconds;
    }
  }, [playback.data]);

  useEffect(() => {
    restorePlaybackPosition();
  }, [restorePlaybackPosition]);

  useEffect(() => {
    const initialEngine = engineRef.current
      ?? (videoRef.current ? new NativeVideoEngine(videoRef.current) : null);
    if (!initialEngine || !streamUrl) return;
    const runtime = runtimeRef.current ?? new LuxPlayerRuntime();
    runtimeRef.current = runtime;
    engineRef.current = initialEngine;
    let activeEngine: PlaybackEngine = initialEngine;
    let performanceElement: HTMLVideoElement | null = null;
    let cancelled = false;
    const handlePerformance = (event: Event) => {
      const performance = (event as CustomEvent<PlaybackPerformance | null>).detail;
      setFallbackSpeedX(performance && !performance.realtime ? performance.speedX : null);
    };
    const load = async () => {
      try {
        if (playbackPlan?.type === "SERVER_HLS") {
          initialEngine.destroy();
          activeEngine = new HlsVideoEngine(initialEngine.element);
          engineRef.current = activeEngine;
        } else {
          const useMkvFallback = await shouldUseClientMkv(source, initialEngine.element);
          const useHevcFallback =
            !useMkvFallback && (await shouldUseClientHevc(source, initialEngine.element));
          if (useMkvFallback || useHevcFallback) {
            setFallbackLoading(true);
            if (cancelled) return;
            initialEngine.destroy();
            if (useMkvFallback) {
              const { ClientMkvEngine } = await import("./mkv-playback-engine");
              activeEngine = new ClientMkvEngine(initialEngine.element, HEVC_RUNTIME_ASSETS);
            } else {
              const { ClientHevcEngine } = await import("./hevc-playback-engine");
              activeEngine = new ClientHevcEngine(initialEngine.element, HEVC_RUNTIME_ASSETS);
            }
            engineRef.current = activeEngine;
            performanceElement = activeEngine.element;
            performanceElement.addEventListener(PLAYBACK_PERFORMANCE_EVENT, handlePerformance);
          }
        }
        if (cancelled) return;
        await runtime.load(
          new LegacyPlaybackEngineAdapter(
            activeEngine,
            playbackPlan?.type === "SERVER_HLS" ? "hls" : undefined,
          ),
          {
            id: source?.id ?? "",
            url: streamUrl,
            poster,
          },
        );
        if (!cancelled && activeEngine.performance)
          handlePerformance(
            new CustomEvent(PLAYBACK_PERFORMANCE_EVENT, {
              detail: activeEngine.performance,
            }),
          );
      } catch (cause) {
        if (!cancelled) {
          const reason = cause instanceof Error ? cause.message : "未知错误";
          if (playbackPlan?.type === "DIRECT") {
            requestServerFallback(reason);
          } else {
            setFailedStreamUrl(streamUrl);
            setPlaybackError(
              `服务端播放失败：${reason} 请尝试其他版本或使用支持该格式的客户端。`,
            );
          }
        }
      } finally {
        if (!cancelled) setFallbackLoading(false);
      }
    };
    void load();
    return () => {
      cancelled = true;
      performanceElement?.removeEventListener(PLAYBACK_PERFORMANCE_EVENT, handlePerformance);
      runtime.destroy();
      if (runtimeRef.current === runtime) runtimeRef.current = null;
      if (engineRef.current === activeEngine) engineRef.current = null;
    };
  }, [playbackPlan?.type, poster, requestServerFallback, source, streamUrl]);

  // Fullscreen change listener
  useEffect(() => {
    const handleFullscreenChange = () => {
      setIsFullscreen(Boolean(document.fullscreenElement));
    };
    document.addEventListener("fullscreenchange", handleFullscreenChange);
    return () => {
      document.removeEventListener("fullscreenchange", handleFullscreenChange);
    };
  }, []);

  // Show/Hide controls with idle timer
  const resetControlsTimeout = useCallback(() => {
    setControlsVisible(true);
    if (hideControlsTimeoutRef.current) {
      clearTimeout(hideControlsTimeoutRef.current);
    }
    if (playing) {
      hideControlsTimeoutRef.current = setTimeout(() => {
        if (!showSettings && !isDraggingScrubberRef.current) {
          setControlsVisible(false);
        }
      }, AUTO_HIDE_DELAY_MS);
    }
  }, [playing, showSettings]);

  const showCenterSplash = (type: "play" | "pause") => {
    setCenterSplash(type);
    if (splashTimeoutRef.current) clearTimeout(splashTimeoutRef.current);
    splashTimeoutRef.current = setTimeout(() => {
      setCenterSplash(null);
    }, 600);
  };

  const togglePlayPause = useCallback(() => {
    const video = videoRef.current;
    if (!video) return;
    if (video.paused) {
      void video.play().then(() => {
        showCenterSplash("play");
      });
    } else {
      video.pause();
      showCenterSplash("pause");
    }
  }, []);

  const seekRelative = useCallback((seconds: number) => {
    const video = videoRef.current;
    if (!video) return;
    const target = Math.max(0, Math.min(video.duration || 0, video.currentTime + seconds));
    video.currentTime = target;
    setCurrentTime(target);
  }, []);

  const toggleMute = useCallback(() => {
    const video = videoRef.current;
    if (!video) return;
    const nextMuted = !video.muted;
    video.muted = nextMuted;
    setIsMuted(nextMuted);
  }, []);

  const changeVolume = useCallback((newVol: number) => {
    const video = videoRef.current;
    if (!video) return;
    const bounded = Math.max(0, Math.min(1, newVol));
    video.volume = bounded;
    video.muted = bounded === 0;
    setVolume(bounded);
    setIsMuted(bounded === 0);
  }, []);

  const toggleFullscreen = useCallback(() => {
    const container = playerContainerRef.current;
    if (!container) return;
    if (!document.fullscreenElement) {
      void container.requestFullscreen().catch(() => undefined);
    } else {
      void document.exitFullscreen().catch(() => undefined);
    }
  }, []);

  const togglePictureInPicture = useCallback(() => {
    const video = videoRef.current;
    if (!video) return;
    if (document.pictureInPictureElement) {
      void document.exitPictureInPicture().catch(() => undefined);
    } else if (document.pictureInPictureEnabled) {
      void video.requestPictureInPicture().catch(() => undefined);
    }
  }, []);

  const changePlaybackRate = useCallback((rate: number) => {
    const video = videoRef.current;
    if (!video) return;
    video.playbackRate = rate;
    setPlaybackRate(rate);
  }, []);

  // Keyboard shortcut listener
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      if (
        target &&
        (target.tagName === "INPUT" ||
          target.tagName === "TEXTAREA" ||
          target.isContentEditable)
      ) {
        return;
      }

      resetControlsTimeout();

      switch (e.key) {
        case " ":
        case "k":
        case "K":
          e.preventDefault();
          togglePlayPause();
          break;
        case "ArrowLeft":
        case "j":
        case "J":
          e.preventDefault();
          seekRelative(-10);
          break;
        case "ArrowRight":
        case "l":
        case "L":
          e.preventDefault();
          seekRelative(10);
          break;
        case "ArrowUp":
          e.preventDefault();
          changeVolume(volume + 0.05);
          break;
        case "ArrowDown":
          e.preventDefault();
          changeVolume(volume - 0.05);
          break;
        case "m":
        case "M":
          e.preventDefault();
          toggleMute();
          break;
        case "f":
        case "F":
          e.preventDefault();
          toggleFullscreen();
          break;
        case "Escape":
          if (showSettings) {
            e.preventDefault();
            setShowSettings(false);
          } else if (!document.fullscreenElement) {
            e.preventDefault();
            if (window.history.length > 1) {
              navigate(-1);
            } else {
              navigate(itemId ? `/items/${encodeURIComponent(itemId)}` : "/");
            }
          }
          break;
      }
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [
    changeVolume,
    itemId,
    navigate,
    resetControlsTimeout,
    seekRelative,
    showSettings,
    toggleFullscreen,
    toggleMute,
    togglePlayPause,
    volume,
  ]);

  // Video event handlers
  const handleLoadedMetadata = () => {
    const video = videoRef.current;
    if (video) {
      setDuration(video.duration || 0);
      setCurrentTime(video.currentTime || 0);
    }
    restorePlaybackPosition();
  };

  const handleTimeUpdate = () => {
    const video = videoRef.current;
    if (!video) return;
    if (!isDraggingScrubberRef.current) {
      setCurrentTime(video.currentTime || 0);
    }
    if (video.duration && !Number.isNaN(video.duration)) {
      setDuration(video.duration);
    }
    if (video.buffered.length > 0) {
      setBufferedEnd(video.buffered.end(video.buffered.length - 1));
    }
    reportPlayback("PLAYING");
  };

  const handlePause = (event: SyntheticEvent<HTMLVideoElement>) => {
    setPlaying(false);
    setControlsVisible(true);
    if (!event.currentTarget.ended) reportPlayback("PAUSED", true);
  };

  // Scrubber scrubbing handlers
  const handleScrubberPointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    const bar = progressBarRef.current;
    const video = videoRef.current;
    if (!bar || !video || !duration) return;

    isDraggingScrubberRef.current = true;
    const updatePosition = (clientX: number) => {
      const rect = bar.getBoundingClientRect();
      const pos = Math.max(0, Math.min(1, (clientX - rect.left) / rect.width));
      const targetTime = pos * duration;
      setCurrentTime(targetTime);
      return targetTime;
    };

    const targetTime = updatePosition(e.clientX);
    video.currentTime = targetTime;

    const handlePointerMove = (moveEvent: MouseEvent) => {
      const time = updatePosition(moveEvent.clientX);
      video.currentTime = time;
    };

    const handlePointerUp = (upEvent: MouseEvent) => {
      const time = updatePosition(upEvent.clientX);
      video.currentTime = time;
      isDraggingScrubberRef.current = false;
      window.removeEventListener("mousemove", handlePointerMove);
      window.removeEventListener("mouseup", handlePointerUp);
      resetControlsTimeout();
    };

    window.addEventListener("mousemove", handlePointerMove);
    window.addEventListener("mouseup", handlePointerUp);
  };

  const handleScrubberMouseMove = (e: ReactMouseEvent<HTMLDivElement>) => {
    const bar = progressBarRef.current;
    if (!bar || !duration) return;
    const rect = bar.getBoundingClientRect();
    const pos = Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width));
    setHoverPercent(pos * 100);
    setHoverTime(pos * duration);
  };

  const handleScrubberMouseLeave = () => {
    setHoverTime(null);
    setHoverPercent(null);
  };

  const handleTimelineKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (event.key === "ArrowLeft") {
      event.preventDefault();
      seekRelative(-5);
    } else if (event.key === "ArrowRight") {
      event.preventDefault();
      seekRelative(5);
    } else if (event.key === "Home") {
      event.preventDefault();
      runtimeRef.current?.seek(0);
      setCurrentTime(0);
    } else if (event.key === "End") {
      event.preventDefault();
      const target = Math.max(0, duration);
      runtimeRef.current?.seek(target);
      setCurrentTime(target);
    }
  };

  const handleBack = () => {
    if (window.history.length > 1) {
      navigate(-1);
    } else {
      navigate(itemId ? `/items/${encodeURIComponent(itemId)}` : "/");
    }
  };

  if (item.isPending) {
    return <PlayerLoadingState message="正在准备播放器…" />;
  }

  if (item.error) {
    return (
      <PlayerErrorState
        title="播放器加载失败"
        message={item.error.message}
        onBack={handleBack}
      />
    );
  }

  if (!media) {
    return (
      <PlayerErrorState title="播放器加载失败" message="媒体条目为空。" onBack={handleBack} />
    );
  }

  if (webPlaybackSession.isPending) {
    return <PlayerLoadingState message="正在创建播放会话…" />;
  }

  if (webPlaybackSession.error) {
    return (
      <PlayerErrorState
        title="播放会话创建失败"
        message={webPlaybackSession.error.message}
        onBack={handleBack}
      />
    );
  }

  const mediaBadgeText = getMediaBadge(source);
  const subtitleInfo = getSubtitleInfo(media);
  const playbackPlanError = playbackPlan?.type === "UNSUPPORTED"
    ? `浏览器和服务端都无法播放这个媒体源（${playbackPlan.reason}）。请尝试其他版本或使用支持该格式的客户端。`
    : null;
  const sourceOptions: PlayerSourceOption[] = (media.mediaSources ?? []).map((entry, index) => ({
    id: entry.id,
    label: entry.qualityLabel || `版本 ${index + 1}`,
    detail: entry.sourceKind === "STRM_URL" ? "STRM" : entry.container || "直链",
  }));

  return (
    <main
      ref={playerContainerRef}
      className={`lux-player-page ${controlsVisible ? "controls-visible" : "controls-hidden"}`}
      onMouseMove={resetControlsTimeout}
      onTouchStart={resetControlsTimeout}
      onClick={resetControlsTimeout}
    >
      <PlayerVideoSurface
        streamUrl={streamUrl}
        poster={poster}
        title={mediaTitle(media)}
        videoRef={setVideoRef}
        onClick={(event) => {
          event.stopPropagation();
          togglePlayPause();
        }}
        onDoubleClick={(event) => {
          event.stopPropagation();
          toggleFullscreen();
        }}
        onError={() => {
          const engine = engineRef.current;
          const reason = engine?.error?.message;
          if (engine?.kind === "native") {
            requestServerFallback(reason);
          } else {
            setFailedStreamUrl(streamUrl);
            setPlaybackError(
              reason
                ? `客户端解码失败：${reason} 请尝试其他版本或使用支持该格式的客户端。`
                : null,
            );
          }
        }}
        onLoadedMetadata={handleLoadedMetadata}
        onPlay={() => {
          hasStartedRef.current = true;
          setPlaying(true);
          reportPlayback("PLAYING", true);
        }}
        onPause={handlePause}
        onTimeUpdate={handleTimeUpdate}
        onEnded={() => {
          setPlaying(false);
          void Promise.resolve(reportPlayback("STOPPED", true)).finally(() => {
            void stopActiveSession();
          });
        }}
        centerSplash={centerSplash}
        fallbackLoading={fallbackLoading}
        fallbackSpeedX={fallbackSpeedX}
        errorMessage={playbackError ?? playbackPlanError}
        showError={failedStreamUrl === streamUrl || !streamUrl}
        onRetry={() => window.location.reload()}
        onBack={handleBack}
      />

      {/* Floating Vignette Shadows */}
      <div className="lux-player-vignette-top" aria-hidden="true" />
      <div className="lux-player-vignette-bottom" aria-hidden="true" />

      <PlayerTopBar
        title={mediaTitle(media)}
        badge={mediaBadgeText}
        subtitle={subtitleInfo}
        sources={sourceOptions}
        selectedSourceId={source?.id ?? ""}
        settingsOpen={showSettings}
        fullscreen={isFullscreen}
        onBack={handleBack}
        onSourceChange={(sourceId) => setSearchParams({ sourceId })}
        onToggleSettings={() => setShowSettings((visible) => !visible)}
        onToggleFullscreen={toggleFullscreen}
      />

      {showSettings ? (
        <PlayerSettingsPanel
          playbackRates={PLAYBACK_SPEEDS}
          playbackRate={playbackRate}
          onChangeRate={changePlaybackRate}
          onClose={() => setShowSettings(false)}
        />
      ) : null}

      <PlayerControls
        playing={playing}
        currentTime={currentTime}
        duration={duration}
        bufferedEnd={bufferedEnd}
        volume={volume}
        muted={isMuted}
        playbackRate={playbackRate}
        fullscreen={isFullscreen}
        pictureInPictureEnabled={Boolean(document.pictureInPictureEnabled)}
        remainingTime={isRemainingTime}
        hoverTime={hoverTime}
        hoverPercent={hoverPercent}
        progressBarRef={progressBarRef}
        onTimelinePointerDown={handleScrubberPointerDown}
        onTimelineMouseMove={handleScrubberMouseMove}
        onTimelineMouseLeave={handleScrubberMouseLeave}
        onTimelineKeyDown={handleTimelineKeyDown}
        onTogglePlayPause={togglePlayPause}
        onSeekRelative={seekRelative}
        onToggleMute={toggleMute}
        onVolumeChange={changeVolume}
        onToggleRemainingTime={() => setIsRemainingTime((remaining) => !remaining)}
        onCycleRate={() => {
          const nextIndex = (PLAYBACK_SPEEDS.indexOf(playbackRate) + 1) % PLAYBACK_SPEEDS.length;
          changePlaybackRate(PLAYBACK_SPEEDS[nextIndex]);
        }}
        onTogglePictureInPicture={togglePictureInPicture}
        onToggleFullscreen={toggleFullscreen}
      />
    </main>
  );
}
