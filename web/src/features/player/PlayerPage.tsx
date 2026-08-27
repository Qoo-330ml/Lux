import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent,
  type PointerEvent as ReactPointerEvent,
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
import { LuxPlayer } from "./components/lux-player";
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

function timelinePosition(bar: HTMLDivElement, clientX: number, duration: number) {
  const rect = bar.getBoundingClientRect();
  if (rect.width <= 0 || duration <= 0) return 0;
  const progress = Math.max(0, Math.min(1, (clientX - rect.left) / rect.width));
  return progress * duration;
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
  const [isPointerInteracting, setIsPointerInteracting] = useState(false);
  const [controlActivity, setControlActivity] = useState(0);
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
  const playbackDataRef = useRef(playback.data);
  playbackDataRef.current = playback.data;

  const media = item.data;
  const source =
    media?.mediaSources?.find((entry) => entry.id === requestedSourceId) ??
    media?.mediaSources?.find((entry) => entry.isDefault) ??
    media?.mediaSources?.[0];
  const playbackKey = `${itemId}:${source?.id ?? ""}:${playbackAttempt}`;
  const [sessionGateKey, setSessionGateKey] = useState(playbackKey);
  const sessionStartedRef = useRef(false);
  const playbackSessionIdRef = useRef<string | null>(null);
  const capabilities = webPlaybackCapabilities(source, playbackAttempt);
  const webPlaybackSession = useQuery({
    queryKey: queryKeys.webPlaybackSession(itemId, source?.id ?? "", playbackAttempt),
    queryFn: () => api.createWebPlaybackSession(itemId, source?.id ?? "", capabilities),
    enabled: Boolean(itemId && source?.id)
      && (sessionGateKey === playbackKey
        || (!sessionStartedRef.current && playbackSessionIdRef.current === null)),
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
  const splashTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const progressBarRef = useRef<HTMLDivElement>(null);
  const isDraggingScrubberRef = useRef(false);
  const scrubberPointerIdRef = useRef<number | null>(null);
  const playbackSequenceRef = useRef(0);
  const fallbackRequestedRef = useRef(false);
  const sessionTransitionRef = useRef(Promise.resolve());
  const fallbackGenerationRef = useRef(0);

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
      sessionIdOverride?: string | null,
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
      const sessionId = sessionIdOverride ?? playbackSessionIdRef.current;
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

  const stopActiveSession = useCallback((sessionId: string | null, keepalive = false) => {
    if (!sessionId) return Promise.resolve();
    return api.stopWebPlaybackSession(sessionId, keepalive).catch(() => undefined);
  }, []);

  const requestServerFallback = useCallback(async (reason?: string) => {
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
    const fallbackGeneration = fallbackGenerationRef.current;
    if (sessionId) await stopActiveSession(sessionId);
    if (fallbackGeneration !== fallbackGenerationRef.current) return;
    if (playbackSessionIdRef.current === sessionId) {
      playbackSessionIdRef.current = null;
      playbackSequenceRef.current = 0;
    }
    setPlaybackError("浏览器无法播放这个媒体源，正在准备兼容的服务端播放…");
    setFailedStreamUrl(null);
    setPlaybackAttempt(1);
  }, [directProxyFallbackRequested, directProxyUrl, playbackAttempt, playbackPlan?.type, stopActiveSession, streamUrl]);

  useEffect(() => {
    fallbackGenerationRef.current += 1;
    lastProgressReportRef.current = 0;
    hasStartedRef.current = false;
    hasRestoredPositionRef.current = false;
    fallbackRequestedRef.current = false;
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
    if (sessionGateKey === playbackKey) return;
    const previousSessionId = playbackSessionIdRef.current;
    playbackSessionIdRef.current = null;
    playbackSequenceRef.current = 0;
    let active = true;
    sessionTransitionRef.current = sessionTransitionRef.current
      .catch(() => undefined)
      .then(() => previousSessionId ? stopActiveSession(previousSessionId) : undefined)
      .finally(() => {
        if (active) setSessionGateKey(playbackKey);
      });
    return () => {
      active = false;
    };
  }, [playbackKey, sessionGateKey, stopActiveSession]);

  useEffect(() => {
    if (sessionGateKey !== playbackKey) return;
    if (webPlaybackSession.data?.sessionId) sessionStartedRef.current = true;
    playbackSessionIdRef.current = webPlaybackSession.data?.sessionId ?? null;
    playbackSequenceRef.current = 0;
  }, [playbackKey, sessionGateKey, webPlaybackSession.data?.sessionId]);

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
      const sessionId = playbackSessionIdRef.current;
      void Promise.resolve(reportPlayback("STOPPED", true, true, undefined, sessionId)).finally(() => {
        void stopActiveSession(sessionId, true);
      });
    };
    window.addEventListener("pagehide", handlePageHide);
    return () => {
      window.removeEventListener("pagehide", handlePageHide);
      const sessionId = playbackSessionIdRef.current;
      void Promise.resolve(reportPlayback("STOPPED", true, false, lastVideoRef.current, sessionId)).finally(() => {
        void stopActiveSession(sessionId);
      });
    };
  }, [reportPlayback, stopActiveSession]);

  const restorePlaybackPosition = useCallback(() => {
    if (hasRestoredPositionRef.current) return;
    const video = videoRef.current;
    const playbackData = playbackDataRef.current;
    if (!video || !playbackData) return;
    if (video.readyState < 1 && !Number.isFinite(video.duration)) return;
    hasRestoredPositionRef.current = true;
    const resumeTicks = playbackData.positionTicks ?? 0;
    if (playbackData.isPlayed || resumeTicks <= 0) return;
    const resumeSeconds = resumeTicks / TICKS_PER_SECOND;
    if (!Number.isFinite(video.duration) || resumeSeconds < video.duration) {
      video.currentTime = resumeSeconds;
    }
  }, []);

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
    const syncSnapshot = (snapshot: {
      currentTime: number;
      duration: number | null;
      bufferedEnd: number;
    }) => {
      if (!isDraggingScrubberRef.current) setCurrentTime(snapshot.currentTime);
      setDuration(snapshot.duration ?? 0);
      setBufferedEnd(snapshot.bufferedEnd);
    };
    const removeRuntimeSubscription = runtime.subscribeEvents((event) => {
      if (cancelled) return;
      switch (event.type) {
        case "SOURCE_READY":
          syncSnapshot(event.snapshot);
          restorePlaybackPosition();
          break;
        case "PLAYING":
          hasStartedRef.current = true;
          setPlaying(true);
          setControlsVisible(true);
          setControlActivity((activity) => activity + 1);
          void reportPlayback("PLAYING", true, false, initialEngine.element);
          break;
        case "PAUSED":
          if (event.snapshot) syncSnapshot(event.snapshot);
          setPlaying(false);
          setControlsVisible(true);
          if (!event.snapshot?.ended) {
            void reportPlayback("PAUSED", true, false, initialEngine.element);
          }
          break;
        case "WAITING":
          setControlsVisible(true);
          break;
        case "SEEK_START":
          setCurrentTime(event.position);
          break;
        case "SEEKED":
          syncSnapshot(event.snapshot);
          break;
        case "TIME_UPDATE":
          syncSnapshot(event.snapshot);
          void reportPlayback("PLAYING", false, false, initialEngine.element);
          break;
        case "ENDED": {
          syncSnapshot(event.snapshot);
          setPlaying(false);
          const sessionId = playbackSessionIdRef.current;
          void Promise.resolve(reportPlayback("STOPPED", true, false, initialEngine.element, sessionId)).finally(() => {
            void stopActiveSession(sessionId);
          });
          break;
        }
        case "ERROR":
          if (activeEngine.kind === "native" && playbackPlan?.type === "DIRECT") {
            void requestServerFallback(event.error.message);
          } else {
            setFailedStreamUrl(streamUrl);
            setPlaybackError(`服务端播放失败：${event.error.message} 请尝试其他版本或使用支持该格式的客户端。`);
          }
          break;
        case "CAN_PLAY":
          break;
      }
    });
    const handlePerformance = (event: Event) => {
      if (cancelled) return;
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
              if (cancelled) return;
              activeEngine = new ClientMkvEngine(initialEngine.element, HEVC_RUNTIME_ASSETS);
            } else {
              const { ClientHevcEngine } = await import("./hevc-playback-engine");
              if (cancelled) return;
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
          if (runtime.state.status === "FAILED") return;
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
      removeRuntimeSubscription();
      runtime.destroy();
      if (runtimeRef.current === runtime) runtimeRef.current = null;
      if (engineRef.current === activeEngine) engineRef.current = null;
    };
  }, [playbackKey, playbackPlan?.type, poster, requestServerFallback, source, streamUrl]);

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

  // Show or reset the controls. The effect below owns the actual timer so
  // concurrent mouse, touch, and keyboard events cannot leave stale timers.
  const resetControlsTimeout = useCallback(() => {
    setControlsVisible(true);
    setControlActivity((activity) => activity + 1);
  }, []);

  useEffect(() => {
    if (!playing || !controlsVisible || showSettings || isPointerInteracting) return;
    const timeout = window.setTimeout(() => setControlsVisible(false), AUTO_HIDE_DELAY_MS);
    return () => window.clearTimeout(timeout);
  }, [controlActivity, controlsVisible, isPointerInteracting, playing, showSettings]);

  useEffect(() => () => {
    if (splashTimeoutRef.current) clearTimeout(splashTimeoutRef.current);
  }, []);

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

  const seekTo = useCallback((seconds: number) => {
    const video = videoRef.current;
    if (!video) return;
    const maximum = Number.isFinite(video.duration) && video.duration > 0
      ? video.duration
      : Math.max(0, duration);
    const target = Math.max(0, Math.min(maximum, seconds));
    if (runtimeRef.current) runtimeRef.current.seek(target);
    else video.currentTime = target;
    setCurrentTime(target);
  }, [duration]);

  const seekRelative = useCallback((seconds: number) => {
    const video = videoRef.current;
    if (!video) return;
    seekTo(video.currentTime + seconds);
  }, [seekTo]);

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

  // Scrubber scrubbing handlers
  const handleScrubberPointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    const bar = progressBarRef.current;
    if (
      !bar
      || !duration
      || scrubberPointerIdRef.current !== null
      || (e.pointerType === "mouse" && e.button !== 0)
    ) return;

    isDraggingScrubberRef.current = true;
    scrubberPointerIdRef.current = e.pointerId;
    setIsPointerInteracting(true);
    resetControlsTimeout();
    try {
      bar.setPointerCapture?.(e.pointerId);
    } catch {
      // A cancelled pointer can no longer be captured; the timeline still
      // receives its element-scoped fallback events.
    }
    seekTo(timelinePosition(bar, e.clientX, duration));
  };

  const handleScrubberPointerMove = (event: ReactPointerEvent<HTMLDivElement>) => {
    const bar = progressBarRef.current;
    if (!bar || scrubberPointerIdRef.current !== event.pointerId) return;
    seekTo(timelinePosition(bar, event.clientX, duration));
  };

  const finishScrubberPointer = (
    event: ReactPointerEvent<HTMLDivElement>,
    commitPosition: boolean,
  ) => {
    const bar = progressBarRef.current;
    if (!bar || scrubberPointerIdRef.current !== event.pointerId) return;
    try {
      if (event.currentTarget.hasPointerCapture?.(event.pointerId)) {
        event.currentTarget.releasePointerCapture?.(event.pointerId);
      }
    } catch {
      // Browsers may release capture before a pointercancel reaches React.
    }
    isDraggingScrubberRef.current = false;
    scrubberPointerIdRef.current = null;
    setIsPointerInteracting(false);
    if (commitPosition) seekTo(timelinePosition(bar, event.clientX, duration));
    resetControlsTimeout();
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
      seekTo(0);
    } else if (event.key === "End") {
      event.preventDefault();
      seekTo(duration);
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
    <LuxPlayer
      containerRef={playerContainerRef}
      controlsVisible={controlsVisible}
      onActivity={resetControlsTimeout}
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
        gestureOptions={{
          currentTime,
          duration,
          volume,
          onSeekTo: seekTo,
          onVolumeChange: changeVolume,
          onSeekRelative: seekRelative,
          onSingleTap: togglePlayPause,
          onActivity: resetControlsTimeout,
          onInteractionChange: setIsPointerInteracting,
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
        onTimelinePointerMove={handleScrubberPointerMove}
        onTimelinePointerUp={(event) => finishScrubberPointer(event, true)}
        onTimelinePointerCancel={(event) => finishScrubberPointer(event, false)}
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
    </LuxPlayer>
  );
}
