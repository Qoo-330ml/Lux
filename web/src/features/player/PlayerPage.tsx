import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent,
  type PointerEvent as ReactPointerEvent,
} from "react";
import { useNavigate, useParams, useSearchParams } from "react-router-dom";
import { ApiError, api } from "../../lib/api/client";
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
import { normalizeCaptionOffset } from "./caption-offset";
import { HlsVideoEngine } from "./hls-playback-engine";
import { canUseHls } from "./hls-capabilities";
import { isRemoteHttpStrmSource, shouldUseClientHevc, shouldUseClientMkv } from "./playback-selection";
import { LegacyPlaybackEngineAdapter } from "./core/legacy-engine-adapter";
import { LuxPlayerRuntime } from "./core/player-runtime";
import { PlayerControls } from "./components/player-controls";
import type { PlayerEpisodeNavigation } from "./components/player-controls";
import { PlayerSettingsPanel } from "./components/player-settings-panel";
import type { PlayerSettingsSourceOption } from "./components/player-settings-panel";
import { PlayerErrorState, PlayerLoadingState } from "./components/player-state";
import { LuxPlayer } from "./components/lux-player";
import { PlayerTopBar } from "./components/player-top-bar";
import { PlayerVideoSurface } from "./components/player-video-surface";
import type {
  PlayerAspectRatio,
  PlayerFlip,
} from "./components/player-presentation";
import { PlayerCaptionOverlay } from "./components/player-caption-overlay";
import { PlayerDanmakuOverlay } from "./components/player-danmaku-overlay";
import { usePlayerAirPlay } from "./components/player-airplay";
import { PlayerMiniProgressBar } from "./components/player-mini-progress";
import { normalizePlayerChapters } from "./player-chapters";
import { createPlaybackTimelineScheduler } from "./player-timeline-scheduler";
import {
  classifyPlayerEngineFailure,
  playerFailure,
  type PlayerFailure,
} from "./components/player-diagnostics";
import { usePlayerPlatform } from "./components/player-platform";
import {
  defaultCaptionSelection,
  nativeCaptionTrack,
  overlayCaptionSource,
  playerCaptionOptions,
  type PlayerRuntimeCaptionTrack,
} from "./components/player-captions";

const TICKS_PER_SECOND = 10_000_000;
const PROGRESS_REPORT_INTERVAL_MS = 10_000;
const TIMELINE_UI_UPDATE_INTERVAL_MS = 100;
const AUTO_HIDE_DELAY_MS = 3_000;
const PLAYBACK_SPEEDS = [0.5, 0.75, 1.0, 1.25, 1.5, 2.0];

const HEVC_RUNTIME_ASSETS = {
  workerUrl: "/hevc/transcode-worker.js",
  wasmUrl: "/hevc/hevc-decode.js",
  wasmModuleUrl: "/hevc/hevc-decode-module.js",
  wasmBinaryUrl: "/hevc/hevc-decode.wasm",
};

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

function screenshotFileName(title: string) {
  const normalized = title
    .replace(/[\\/:*?"<>|]/g, "-")
    .replace(/\s+/g, " ")
    .trim()
    .slice(0, 80);
  return `${normalized || "lux-screenshot"}-${new Date().toISOString().replace(/[:.]/g, "-")}.png`;
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

export function episodeNavigationFor(episodes: MediaItem[], currentEpisodeId: string): PlayerEpisodeNavigation {
  const playableEpisodes = episodes.filter((episode) =>
    episode.itemType === "EPISODE" && (episode.mediaSources?.length ?? 0) > 0,
  );
  const currentIndex = playableEpisodes.findIndex((episode) => episode.id === currentEpisodeId);
  return {
    previousEpisodeId: currentIndex > 0 ? playableEpisodes[currentIndex - 1].id : null,
    nextEpisodeId: currentIndex >= 0 && currentIndex < playableEpisodes.length - 1
      ? playableEpisodes[currentIndex + 1].id
      : null,
  };
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
  const [playbackFailure, setPlaybackFailure] = useState<PlayerFailure | null>(null);
  const [fallbackLoading, setFallbackLoading] = useState(false);
  const [fallbackSpeedX, setFallbackSpeedX] = useState<number | null>(null);

  // Playback control states
  const [currentTime, setCurrentTime] = useState(0);
  const [duration, setDuration] = useState(0);
  const [bufferedEnd, setBufferedEnd] = useState(0);
  const [volume, setVolume] = useState(1);
  const [isMuted, setIsMuted] = useState(false);
  const [playbackRate, setPlaybackRate] = useState(1.0);
  const [loopPlayback, setLoopPlayback] = useState(false);
  const [aspectRatio, setAspectRatio] = useState<PlayerAspectRatio>("default");
  const [flip, setFlip] = useState<PlayerFlip>("normal");
  const [isFullscreen, setIsFullscreen] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [isRemainingTime, setIsRemainingTime] = useState(false);
  const [controlsVisible, setControlsVisible] = useState(true);
  const [isPointerInteracting, setIsPointerInteracting] = useState(false);
  const [controlActivity, setControlActivity] = useState(0);
  const [centerSplash, setCenterSplash] = useState<"play" | "pause" | null>(null);
  const [hoverTime, setHoverTime] = useState<number | null>(null);
  const [hoverPercent, setHoverPercent] = useState<number | null>(null);
  const [danmuVisible, setDanmuVisible] = useState(true);
  const [screenshotStatus, setScreenshotStatus] = useState<string | null>(null);
  const [selectedCaptionStreamIndex, setSelectedCaptionStreamIndex] = useState<number | null>(null);
  const [captionSourceId, setCaptionSourceId] = useState<string | null>(null);
  const [captionStatus, setCaptionStatus] = useState<string | null>(null);
  const [captionOffset, setCaptionOffset] = useState(0);
  const [nativeCaptionTracks, setNativeCaptionTracks] = useState<PlayerRuntimeCaptionTrack[]>([]);
  const [airPlayVideo, setAirPlayVideo] = useState<HTMLVideoElement | null>(null);
  const currentTimeRef = useRef(0);
  const durationRef = useRef(0);
  const bufferedEndRef = useRef(0);

  const requestedSourceId = searchParams.get("sourceId");
  const bootstrapKey = `${itemId}:${requestedSourceId ?? ""}:${playbackAttempt}`;
  const [sessionGateKey, setSessionGateKey] = useState(bootstrapKey);
  const bootstrapCapabilities = webPlaybackCapabilities(undefined, playbackAttempt);
  const playbackBootstrap = useQuery({
    queryKey: queryKeys.playbackBootstrap(itemId, requestedSourceId, playbackAttempt),
    queryFn: ({ signal }) => api.createWebPlaybackBootstrap(
      itemId,
      requestedSourceId ?? undefined,
      bootstrapCapabilities,
      signal,
    ),
    enabled: Boolean(itemId) && playbackAttempt === 0 && sessionGateKey === bootstrapKey,
    retry: false,
    staleTime: Number.POSITIVE_INFINITY,
    placeholderData: (previous) => previous,
  });
  const useLegacyPlaybackQueries = playbackAttempt !== 0 || playbackBootstrap.isError;
  const item = useQuery({
    queryKey: queryKeys.item(itemId),
    queryFn: () => api.item(itemId),
    enabled: Boolean(itemId) && useLegacyPlaybackQueries,
  });
  const playback = useQuery({
    queryKey: queryKeys.playback(itemId),
    queryFn: () => api.playback(itemId),
    enabled: Boolean(itemId) && useLegacyPlaybackQueries,
  });
  const playbackData = playbackBootstrap.isPlaceholderData
    ? playback.data
    : playbackBootstrap.data?.playback ?? playback.data;
  const playbackDataRef = useRef(playbackData);
  playbackDataRef.current = playbackData;

  const media = playbackBootstrap.data?.item ?? item.data;
  const episodeSiblings = useQuery({
    queryKey: queryKeys.children(media?.seriesId ?? "", "EPISODE", media?.parentId ?? undefined),
    queryFn: () => api.children(media?.seriesId ?? "", {
      itemType: "EPISODE",
      seasonId: media?.parentId ?? undefined,
    }),
    enabled: media?.itemType === "EPISODE" && Boolean(media.seriesId) && Boolean(media.parentId),
    staleTime: 60_000,
  });
  const episodeNavigation = media?.itemType === "EPISODE"
    ? episodeNavigationFor(episodeSiblings.data?.items ?? [], media.id)
    : null;
  const source =
    media?.mediaSources?.find((entry) => entry.id === requestedSourceId) ??
    media?.mediaSources?.find((entry) => entry.isDefault) ??
    media?.mediaSources?.[0];
  const nativeCaptionTracksSupported = typeof HTMLTrackElement !== "undefined";
  const captionOptions = playerCaptionOptions(source, nativeCaptionTracksSupported, nativeCaptionTracks);
  const selectedCaptionOption = captionSourceId === source?.id
    ? captionOptions.find((caption) => caption.streamIndex === selectedCaptionStreamIndex && caption.available) ?? null
    : null;
  const captionTrack = nativeCaptionTrack(itemId, source?.id ?? "", selectedCaptionOption);
  const captionOverlaySource = overlayCaptionSource(itemId, source?.id ?? "", selectedCaptionOption);
  const nativeCaptionTrackId = selectedCaptionOption?.renderMode === "native-inband"
    ? selectedCaptionOption.runtimeTrackId ?? null
    : null;
  // For the first bootstrap request, keep the key tied to the URL selection
  // (or the default slot) rather than changing it when the response resolves
  // its default source. This prevents the newly-created bootstrap session from
  // being mistaken for an old session during the initial render.
  const playbackKeySourceId = playbackAttempt === 0 && !requestedSourceId
    ? ""
    : source?.id ?? requestedSourceId ?? "";
  const playbackKey = `${itemId}:${playbackKeySourceId}:${playbackAttempt}`;
  const sessionStartedRef = useRef(false);
  const playbackSessionIdRef = useRef<string | null>(null);
  const capabilities = webPlaybackCapabilities(source, playbackAttempt);
  const webPlaybackSession = useQuery({
    queryKey: queryKeys.webPlaybackSession(itemId, source?.id ?? "", playbackAttempt),
    queryFn: ({ signal }) => api.createWebPlaybackSession(
      itemId,
      source?.id ?? "",
      capabilities,
      signal,
    ),
    enabled: Boolean(itemId && source?.id) && useLegacyPlaybackQueries
      && (sessionGateKey === playbackKey
        || (!sessionStartedRef.current && playbackSessionIdRef.current === null)),
    retry: false,
    staleTime: Number.POSITIVE_INFINITY,
  });
  const playbackSession = playbackBootstrap.isPlaceholderData
    ? webPlaybackSession.data
    : playbackBootstrap.data?.session ?? webPlaybackSession.data;
  const playbackPlan = playbackSession?.plan;
  const directProxyUrl = playbackPlan?.type === "DIRECT" ? playbackPlan.proxyUrl : undefined;
  const streamUrl = playbackPlan?.type === "DIRECT"
    ? (directProxyFallbackRequested ? playbackPlan.url : directProxyUrl ?? playbackPlan.url)
    : playbackPlan?.type === "SERVER_HLS"
      ? playbackPlan.manifestUrl
      : "";
  const poster = media ? imageUrl(media, "fanart") ?? imageUrl(media) : null;
  const chapterTimeline = useMemo(
    () => normalizePlayerChapters(source?.chapters, duration),
    [duration, source?.chapters],
  );
  const activeIntroRange = chapterTimeline.introRanges.find(
    (range) => currentTime >= range.start && currentTime < range.end,
  ) ?? null;

  const playerContainerRef = useRef<HTMLDivElement>(null);
  const videoRef = useRef<HTMLVideoElement>(null);
  const lastVideoRef = useRef<HTMLVideoElement | null>(null);
  const engineRef = useRef<PlaybackEngine | null>(null);
  const runtimeRef = useRef<LuxPlayerRuntime | null>(null);
  const lastProgressReportRef = useRef(0);
  const hasStartedRef = useRef(false);
  const hasRestoredPositionRef = useRef(false);
  const splashTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const screenshotStatusTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const progressBarRef = useRef<HTMLDivElement>(null);
  const isDraggingScrubberRef = useRef(false);
  const scrubberPointerIdRef = useRef<number | null>(null);
  const playbackSequenceRef = useRef(0);
  const fallbackRequestedRef = useRef(false);
  const sessionTransitionRef = useRef(Promise.resolve());
  const fallbackGenerationRef = useRef(0);
  const captionSelectionTouchedRef = useRef(false);
  const airPlay = usePlayerAirPlay(airPlayVideo, playbackKey);

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
      setNativeCaptionTracks([]);
      setAirPlayVideo(null);
      return;
    }
    videoRef.current = video;
    lastVideoRef.current = video;
    setAirPlayVideo(video);
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
          .then(() => {
            void queryClient.invalidateQueries({ queryKey: queryKeys.home });
          })
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

  const requestServerFallback = useCallback(async (reason?: unknown) => {
    if (
      playbackPlan?.type === "DIRECT"
      && directProxyUrl
      && !directProxyFallbackRequested
    ) {
      setDirectProxyFallbackRequested(true);
      setFailedStreamUrl(null);
      setPlaybackFailure(null);
      return;
    }
    if (
      playbackAttempt !== 0 ||
      playbackPlan?.type !== "DIRECT" ||
      fallbackRequestedRef.current
    ) {
      setFailedStreamUrl(streamUrl || null);
      setPlaybackFailure(classifyPlayerEngineFailure(reason));
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
    setPlaybackFailure(null);
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
    setPlaybackFailure(null);
    setFallbackLoading(false);
    setFallbackSpeedX(null);
    currentTimeRef.current = 0;
    durationRef.current = 0;
    bufferedEndRef.current = 0;
    setCurrentTime(0);
    setDuration(0);
    setBufferedEnd(0);
  }, [itemId, requestedSourceId]);

  useEffect(() => {
    captionSelectionTouchedRef.current = false;
    setNativeCaptionTracks([]);
    const initialCaption = defaultCaptionSelection(
      playerCaptionOptions(source, nativeCaptionTracksSupported, []),
    );
    setCaptionSourceId(source?.id ?? null);
    setSelectedCaptionStreamIndex(initialCaption?.streamIndex ?? null);
    setCaptionStatus(null);
  }, [nativeCaptionTracksSupported, source?.id]);

  const defaultCaptionStreamIndex = defaultCaptionSelection(captionOptions)?.streamIndex ?? null;
  useEffect(() => {
    if (
      captionSelectionTouchedRef.current
      || !source?.id
      || captionSourceId !== source.id
    ) {
      return;
    }
    setSelectedCaptionStreamIndex((previous) => (
      previous === defaultCaptionStreamIndex ? previous : defaultCaptionStreamIndex
    ));
  }, [captionSourceId, defaultCaptionStreamIndex, source?.id]);

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
    if (playbackSession?.sessionId) sessionStartedRef.current = true;
    playbackSessionIdRef.current = playbackSession?.sessionId ?? null;
    playbackSequenceRef.current = 0;
  }, [playbackKey, playbackSession?.sessionId, sessionGateKey]);

  useEffect(() => {
    const sessionId = playbackSession?.sessionId;
    if (!sessionId) return;
    const heartbeat = window.setInterval(() => {
      void api.webPlaybackHeartbeat(sessionId).catch(() => undefined);
    }, 60_000);
    return () => window.clearInterval(heartbeat);
  }, [playbackSession?.sessionId]);

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
    const timelineScheduler = createPlaybackTimelineScheduler(
      (snapshot) => {
        if (!isDraggingScrubberRef.current) setCurrentTime(snapshot.currentTime);
        setDuration(snapshot.duration);
        setBufferedEnd(snapshot.bufferedEnd);
      },
      undefined,
      undefined,
      { minIntervalMs: TIMELINE_UI_UPDATE_INTERVAL_MS },
    );
    const syncSnapshot = (snapshot: {
      currentTime: number;
      duration: number | null;
      bufferedEnd?: number;
    }, immediate = false) => {
      const ranges = activeEngine.element.buffered;
      const next = {
        currentTime: snapshot.currentTime,
        duration: snapshot.duration ?? 0,
        bufferedEnd: snapshot.bufferedEnd
          ?? (ranges.length > 0 ? ranges.end(ranges.length - 1) : snapshot.currentTime),
      };
      currentTimeRef.current = next.currentTime;
      durationRef.current = next.duration;
      bufferedEndRef.current = next.bufferedEnd;
      timelineScheduler.schedule(next, immediate);
    };
    const removeRuntimeSubscription = runtime.subscribeEvents((event) => {
      if (cancelled) return;
      switch (event.type) {
        case "SOURCE_READY":
          syncSnapshot(event.snapshot, true);
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
          if (event.snapshot) syncSnapshot(event.snapshot, true);
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
          currentTimeRef.current = event.position;
          setCurrentTime(event.position);
          break;
        case "SEEKED":
          syncSnapshot(event.snapshot, true);
          break;
        case "TIME_UPDATE":
          syncSnapshot(event.snapshot);
          void reportPlayback("PLAYING", false, false, initialEngine.element);
          break;
        case "ENDED": {
          syncSnapshot(event.snapshot, true);
          if (initialEngine.element.loop) {
            runtime.seek(0);
            currentTimeRef.current = 0;
            setCurrentTime(0);
            void runtime.play().catch((cause) => {
              if (!cancelled) {
                setFailedStreamUrl(streamUrl);
                setPlaybackFailure(classifyPlayerEngineFailure(cause));
              }
            });
            break;
          }
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
            setPlaybackFailure(classifyPlayerEngineFailure(event.error));
          }
          break;
        case "CAN_PLAY":
          syncSnapshot(activeEngine.snapshot(), true);
          break;
      }
    });
    const handlePerformance = (event: Event) => {
      if (cancelled) return;
      const performance = (event as CustomEvent<PlaybackPerformance | null>).detail;
      setFallbackSpeedX(performance && !performance.realtime ? performance.speedX : null);
    };
    const handleDurationChange = () => {
      if (!cancelled) syncSnapshot(activeEngine.snapshot(), true);
    };
    initialEngine.element.addEventListener("durationchange", handleDurationChange);
    const load = async () => {
      try {
        if (playbackPlan?.type === "SERVER_HLS") {
          initialEngine.destroy();
          const { HlsVideoEngine } = await import("./hls-playback-engine");
          if (cancelled) return;
          activeEngine = new HlsVideoEngine(initialEngine.element);
          engineRef.current = activeEngine;
        } else {
          // A remote HTTP(S) STRM must remain on the native element. The
          // signed Lux endpoint follows the upstream redirect, but a client
          // fallback would fetch that redirect with CORS and cannot safely
          // consume the remote response when the upstream omits CORS headers.
          const remoteHttpStrm = isRemoteHttpStrmSource(source);
          const useMkvFallback = remoteHttpStrm
            ? false
            : await shouldUseClientMkv(source, initialEngine.element);
          const useHevcFallback =
            !useMkvFallback
            && !remoteHttpStrm
            && (await shouldUseClientHevc(source, initialEngine.element));
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
          if (playbackPlan?.type === "DIRECT") {
            requestServerFallback(cause);
          } else {
            setFailedStreamUrl(streamUrl);
            setPlaybackFailure(classifyPlayerEngineFailure(cause));
          }
        }
      } finally {
        if (!cancelled) setFallbackLoading(false);
      }
    };
    void load();
    return () => {
      cancelled = true;
      timelineScheduler.dispose();
      initialEngine.element.removeEventListener("durationchange", handleDurationChange);
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
    if (screenshotStatusTimeoutRef.current) clearTimeout(screenshotStatusTimeoutRef.current);
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

  usePlayerPlatform({
    enabled: Boolean(streamUrl),
    title: media?.title ?? media?.name ?? "Lux",
    artist: getSubtitleInfo(media) ?? "Lux",
    playing,
    currentTime,
    duration,
    onPlay: () => {
      const video = videoRef.current;
      if (video?.paused) void video.play();
    },
    onPause: () => videoRef.current?.pause(),
    onSeekRelative: seekRelative,
    onSeekTo: seekTo,
    onVisible: resetControlsTimeout,
  });

  useEffect(() => {
    const showControlsAfterLayoutChange = () => resetControlsTimeout();
    window.addEventListener("orientationchange", showControlsAfterLayoutChange);
    screen.orientation?.addEventListener("change", showControlsAfterLayoutChange);
    return () => {
      window.removeEventListener("orientationchange", showControlsAfterLayoutChange);
      screen.orientation?.removeEventListener("change", showControlsAfterLayoutChange);
    };
  }, [resetControlsTimeout]);

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

  const selectCaption = useCallback((streamIndex: number | null) => {
    captionSelectionTouchedRef.current = true;
    if (streamIndex === null) {
      setCaptionSourceId(source?.id ?? null);
      setSelectedCaptionStreamIndex(null);
      setCaptionStatus(null);
      resetControlsTimeout();
      return;
    }
    const option = captionOptions.find((caption) => caption.streamIndex === streamIndex);
    if (!option?.available) return;
    setCaptionSourceId(source?.id ?? null);
    setSelectedCaptionStreamIndex(option.streamIndex);
    setCaptionStatus(option.renderMode === "native-inband" ? null : "字幕加载中…");
    resetControlsTimeout();
  }, [captionOptions, resetControlsTimeout, source?.id]);

  const changeCaptionOffset = useCallback((offset: number) => {
    setCaptionOffset(normalizeCaptionOffset(offset));
    resetControlsTimeout();
  }, [resetControlsTimeout]);

  const showScreenshotStatus = useCallback((message: string) => {
    setScreenshotStatus(message);
    if (screenshotStatusTimeoutRef.current) clearTimeout(screenshotStatusTimeoutRef.current);
    screenshotStatusTimeoutRef.current = setTimeout(() => setScreenshotStatus(null), 2_500);
  }, []);

  const takeScreenshot = useCallback(() => {
    const video = videoRef.current;
    if (!video || video.videoWidth <= 0 || video.videoHeight <= 0) {
      showScreenshotStatus("当前视频帧尚未就绪");
      return;
    }

    const canvas = document.createElement("canvas");
    canvas.width = video.videoWidth;
    canvas.height = video.videoHeight;
    const context = canvas.getContext("2d");
    if (!context) {
      showScreenshotStatus("当前浏览器无法生成截图");
      return;
    }

    try {
      context.drawImage(video, 0, 0, canvas.width, canvas.height);
      canvas.toBlob((blob) => {
        if (!blob) {
          showScreenshotStatus("当前媒体不允许截图");
          return;
        }
        try {
          const objectUrl = URL.createObjectURL(blob);
          const anchor = document.createElement("a");
          anchor.href = objectUrl;
          anchor.download = screenshotFileName(media ? mediaTitle(media) : "lux-screenshot");
          anchor.click();
          window.setTimeout(() => URL.revokeObjectURL(objectUrl), 0);
          showScreenshotStatus("截图已保存");
        } catch {
          showScreenshotStatus("当前浏览器无法保存截图");
        }
      }, "image/png");
    } catch {
      showScreenshotStatus("当前媒体不允许截图");
    }
  }, [media, showScreenshotStatus]);

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

  const navigateToEpisode = useCallback((episodeId: string) => {
    setShowSettings(false);
    navigate(`/watch/${encodeURIComponent(episodeId)}`);
  }, [navigate]);

  if (playbackBootstrap.isPending || (useLegacyPlaybackQueries && item.isPending)) {
    return <PlayerLoadingState message="正在准备播放器…" />;
  }

  if (useLegacyPlaybackQueries && item.error) {
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

  if (useLegacyPlaybackQueries && webPlaybackSession.isPending) {
    return <PlayerLoadingState message="正在创建播放会话…" />;
  }

  if (useLegacyPlaybackQueries && webPlaybackSession.error) {
    const sessionFailure = webPlaybackSession.error instanceof ApiError
      && [401, 403, 410].includes(webPlaybackSession.error.status)
      ? classifyPlayerEngineFailure(webPlaybackSession.error, webPlaybackSession.error.status)
      : playerFailure("SERVER_PLAN_FAILED");
    return (
      <PlayerErrorState
        title={sessionFailure.title}
        message={sessionFailure.message}
        onBack={handleBack}
      />
    );
  }

  const subtitleInfo = getSubtitleInfo(media);
  const planFailure = playbackPlan?.type === "UNSUPPORTED"
    ? playerFailure("BROWSER_UNSUPPORTED")
    : null;
  const surfaceFailure = playbackFailure
    ?? planFailure
    ?? (!streamUrl ? playerFailure("SERVER_PLAN_FAILED") : null);
  const sourceOptions: PlayerSettingsSourceOption[] = (media.mediaSources ?? []).map((entry, index) => ({
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
        corsEnabled={source?.sourceKind !== "STRM_URL"}
        poster={poster}
        title={mediaTitle(media)}
        videoRef={setVideoRef}
        presentation={{ loop: loopPlayback, aspectRatio, flip }}
        onClick={(event) => {
          event.stopPropagation();
          togglePlayPause();
        }}
        onDoubleClick={(event) => {
          event.stopPropagation();
          toggleFullscreen();
        }}
        playing={playing}
        onTogglePlayback={togglePlayPause}
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
        captionTrack={captionTrack}
        nativeCaptionTrackId={nativeCaptionTrackId}
        onNativeCaptionTracksChange={setNativeCaptionTracks}
        captionOffset={captionOffset}
        captionDuration={duration}
        captionLifecycleKey={playbackKey}
        onCaptionTrackLoad={() => setCaptionStatus(null)}
        onCaptionTrackError={() => setCaptionStatus("字幕加载失败")}
        errorMessage={null}
        failure={surfaceFailure}
        showError={failedStreamUrl === streamUrl || !streamUrl}
        onRetry={() => window.location.reload()}
        onBack={handleBack}
      />
      <PlayerCaptionOverlay
        source={captionOverlaySource}
        currentTime={currentTime}
        captionOffset={captionOffset}
        captionDuration={duration}
        lifecycleKey={playbackKey}
        onStatusChange={setCaptionStatus}
      />
      <PlayerDanmakuOverlay
        itemId={itemId}
        sourceId={source?.id ?? ""}
        visible={danmuVisible}
        currentTime={currentTime}
        playbackRate={playbackRate}
        lifecycleKey={playbackKey}
      />
      <PlayerMiniProgressBar
        controlsVisible={controlsVisible}
        currentTime={currentTime}
        duration={duration}
        bufferedEnd={bufferedEnd}
      />

      {/* Floating Vignette Shadows */}
      <div className="lux-player-vignette-top" aria-hidden="true" />
      <div className="lux-player-vignette-bottom" aria-hidden="true" />

      <PlayerTopBar
        title={mediaTitle(media)}
        subtitle={subtitleInfo}
        onBack={handleBack}
        airPlayAvailable={airPlay.available}
        onAirPlay={airPlay.showPicker}
        pictureInPictureEnabled={Boolean(document.pictureInPictureEnabled)}
        onTogglePictureInPicture={togglePictureInPicture}
      />

      {showSettings ? (
        <PlayerSettingsPanel
          playbackRates={PLAYBACK_SPEEDS}
          playbackRate={playbackRate}
          onChangeRate={changePlaybackRate}
          sources={sourceOptions}
          selectedSourceId={source?.id ?? ""}
          onSourceChange={(sourceId) => setSearchParams({ sourceId })}
          presentation={{
            loop: loopPlayback,
            aspectRatio,
            flip,
            onToggleLoop: () => setLoopPlayback((enabled) => !enabled),
            onChangeAspectRatio: setAspectRatio,
            onChangeFlip: setFlip,
          }}
          captions={captionOptions}
          selectedCaptionStreamIndex={captionSourceId === source?.id ? selectedCaptionStreamIndex : null}
          captionStatus={captionStatus}
          onSelectCaption={selectCaption}
          captionOffset={captionOffset}
          onChangeCaptionOffset={changeCaptionOffset}
          onClose={() => setShowSettings(false)}
        />
      ) : null}

      {screenshotStatus ? (
        <div className="lux-player-screenshot-status" role="status">
          {screenshotStatus}
        </div>
      ) : null}

      <PlayerControls
        playing={playing}
        currentTime={currentTime}
        duration={duration}
        bufferedEnd={bufferedEnd}
        volume={volume}
        muted={isMuted}
        fullscreen={isFullscreen}
        pictureInPictureEnabled={Boolean(document.pictureInPictureEnabled)}
        danmuVisible={danmuVisible}
        episodeNavigation={episodeNavigation}
        airPlayAvailable={airPlay.available}
        chapters={chapterTimeline.segments}
        introSkip={activeIntroRange}
        settingsOpen={showSettings}
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
        onNavigateToEpisode={navigateToEpisode}
        onToggleMute={toggleMute}
        onVolumeChange={changeVolume}
        onToggleRemainingTime={() => setIsRemainingTime((remaining) => !remaining)}
        onToggleDanmu={() => {
          setDanmuVisible((visible) => !visible);
          resetControlsTimeout();
        }}
        onAirPlay={airPlay.showPicker}
        onChapterSeek={seekTo}
        onSkipIntro={seekTo}
        onTakeScreenshot={takeScreenshot}
        onToggleSettings={() => setShowSettings((visible) => !visible)}
        onTogglePictureInPicture={togglePictureInPicture}
        onToggleFullscreen={toggleFullscreen}
      />
    </LuxPlayer>
  );
}
