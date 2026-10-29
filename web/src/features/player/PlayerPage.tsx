import "./player.css";
import { useQuery } from "@tanstack/react-query";
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
import type { WebPlaybackTrackSelection } from "../../lib/api/types";
import { imageUrl, mediaTitle } from "../home/media";
import {
  NativeVideoEngine,
  PLAYBACK_PERFORMANCE_EVENT,
  type PlaybackEngine,
  type PlaybackPerformance,
} from "./playback-engine";
import {
  DIRECT_CANDIDATE_TIMEOUT_MS,
  clearRememberedCandidate,
  directCandidates,
  orderCandidates,
  rememberCandidate,
  rememberedCandidate,
} from "./direct-candidates";
import { canUseRemoteMkvCaptionSidecar, isRemoteHttpStrmSource, shouldUseClientHevc, shouldUseClientMkv } from "./playback-selection";
import { LegacyPlaybackEngineAdapter } from "./core/legacy-engine-adapter";
import { LuxPlayerRuntime } from "./core/player-runtime";
import { PlayerControls } from "./components/player-controls";
import { PlayerSettingsPanel } from "./components/player-settings-panel";
import { PlayerInfoPanel } from "./components/player-info-panel";
import {
  buildPlayerInfoGroups,
  createClientStatsSampler,
  playerInfoCopyText,
  summarizeSource,
  type PlayerClientStats,
} from "./player-stats";
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
import { webPlaybackCapabilities } from "./playback-capabilities";
import { usePlaybackReporting } from "./usePlaybackReporting";
import { useCaptionSelection } from "./useCaptionSelection";
import { useRuntimeCaptionReader } from "./useRuntimeCaptionReader";
import {
  TICKS_PER_SECOND,
  episodeNavigationFor,
  getSubtitleInfo,
  isMatroskaSource,
  screenshotFileName,
  timelinePosition,
  useClientEngine,
} from "./player-page-helpers";

const TIMELINE_UI_UPDATE_INTERVAL_MS = 100;
const AUTO_HIDE_DELAY_MS = 3_000;
const PLAYBACK_SPEEDS = [0.5, 0.75, 1.0, 1.25, 1.5, 2.0];

type PlaybackEngineHandoff = {
  playbackKey: string;
  sourceId: string;
  currentTime: number;
  playing: boolean;
};

const HEVC_RUNTIME_ASSETS = {
  workerUrl: "/hevc/transcode-worker.js",
  wasmUrl: "/hevc/hevc-decode.js",
  wasmModuleUrl: "/hevc/hevc-decode-module.js",
  wasmBinaryUrl: "/hevc/hevc-decode.wasm",
};

export function PlayerPage() {
  const { itemId = "" } = useParams();
  const [searchParams, setSearchParams] = useSearchParams();
  const navigate = useNavigate();

  const [playing, setPlaying] = useState(false);
  const [playbackAttempt, setPlaybackAttempt] = useState(0);
  const [candidateOffset, setCandidateOffset] = useState(0);
  const [failedStreamUrl, setFailedStreamUrl] = useState<string | null>(null);
  const [playbackFailure, setPlaybackFailure] = useState<PlayerFailure | null>(null);
  const [fallbackLoading, setFallbackLoading] = useState(false);
  const [fallbackSpeedX, setFallbackSpeedX] = useState<number | null>(null);
  const [clientEngineActive, setClientEngineActive] = useState(false);

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
  const [showInfo, setShowInfo] = useState(false);
  const [clientStats, setClientStats] = useState<PlayerClientStats | null>(null);
  const [isRemainingTime, setIsRemainingTime] = useState(false);
  const [controlsVisible, setControlsVisible] = useState(true);
  const [isPointerInteracting, setIsPointerInteracting] = useState(false);
  const [controlActivity, setControlActivity] = useState(0);
  const [centerSplash, setCenterSplash] = useState<"play" | "pause" | null>(null);
  const [hoverTime, setHoverTime] = useState<number | null>(null);
  const [hoverPercent, setHoverPercent] = useState<number | null>(null);
  const [danmuVisible, setDanmuVisible] = useState(true);
  const [screenshotStatus, setScreenshotStatus] = useState<string | null>(null);
  const [airPlayVideo, setAirPlayVideo] = useState<HTMLVideoElement | null>(null);
  const currentTimeRef = useRef(0);
  const durationRef = useRef(0);
  const bufferedEndRef = useRef(0);

  const requestedSourceId = searchParams.get("sourceId");
  const requestedAudioStreamIndex = searchParams.get("audioStreamIndex");
  const requestedSubtitleStreamIndex = searchParams.get("subtitleStreamIndex");
  const trackSelection = useMemo<WebPlaybackTrackSelection | undefined>(() => {
    const audioStreamIndex = requestedAudioStreamIndex?.trim() ? Number(requestedAudioStreamIndex) : undefined;
    const subtitleStreamIndex = requestedSubtitleStreamIndex?.trim() ? Number(requestedSubtitleStreamIndex) : undefined;
    return Number.isInteger(audioStreamIndex) || Number.isInteger(subtitleStreamIndex)
      ? {
        ...(Number.isInteger(audioStreamIndex) ? { audioStreamIndex } : {}),
        ...(Number.isInteger(subtitleStreamIndex) ? { subtitleStreamIndex } : {}),
      }
      : undefined;
  }, [requestedAudioStreamIndex, requestedSubtitleStreamIndex]);
  const bootstrapKey = `${itemId}:${requestedSourceId ?? ""}:${requestedAudioStreamIndex ?? ""}:${playbackAttempt}`;
  const [sessionGateKey, setSessionGateKey] = useState(bootstrapKey);
  const bootstrapCapabilities = webPlaybackCapabilities(undefined, playbackAttempt);
  const playbackBootstrap = useQuery({
    queryKey: queryKeys.playbackBootstrap(itemId, requestedSourceId, playbackAttempt, requestedAudioStreamIndex, requestedSubtitleStreamIndex),
    queryFn: ({ signal }) => api.createWebPlaybackBootstrap(
      itemId,
      requestedSourceId ?? undefined,
      bootstrapCapabilities,
      signal,
      trackSelection,
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
  // The controls auto-hide timer is owned by the effect below; this callback only
  // requests "show again", so it can be created before the caption selection hook.
  const resetControlsTimeout = useCallback(() => {
    setControlsVisible(true);
    setControlActivity((activity) => activity + 1);
  }, []);
  const {
    captionOptions,
    selectedCaptionOption,
    selectedCaptionId,
    captionSourceId,
    captionStatus,
    captionOffset,
    runtimeCaptionOverlayCues,
    captionTrack,
    captionOverlaySource,
    nativeCaptionTrackId,
    selectCaption,
    handleRuntimeCaptionCue,
    changeCaptionOffset,
    clearRuntimeCaptionCues,
    setCaptionStatus,
    setNativeCaptionTracks,
  } = useCaptionSelection({
    itemId,
    source,
    nativeCaptionTracksSupported,
    requestedSubtitleStreamIndex,
    resetControlsTimeout,
  });
  useEffect(() => {
    const controller = engineRef.current?.captionController;
    if (!controller) return;
    controller.select(nativeCaptionTrackId);
  }, [nativeCaptionTrackId]);
  // For the first bootstrap request, keep the key tied to the URL selection
  // (or the default slot) rather than changing it when the response resolves
  // its default source. This prevents the newly-created bootstrap session from
  // being mistaken for an old session during the initial render.
  const playbackKeySourceId = playbackAttempt === 0 && !requestedSourceId
    ? ""
    : source?.id ?? requestedSourceId ?? "";
  const playbackKey = `${itemId}:${playbackKeySourceId}:${requestedAudioStreamIndex ?? ""}:${playbackAttempt}`;
  const sessionStartedRef = useRef(false);
  const playbackSessionIdRef = useRef<string | null>(null);
  const capabilities = webPlaybackCapabilities(source, playbackAttempt);
  const webPlaybackSession = useQuery({
    queryKey: queryKeys.webPlaybackSession(itemId, source?.id ?? "", playbackAttempt, requestedAudioStreamIndex, requestedSubtitleStreamIndex),
    queryFn: ({ signal }) => api.createWebPlaybackSession(
      itemId,
      source?.id ?? "",
      capabilities,
      signal,
      trackSelection,
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
  // The information panel polls the server only while it is open: one request per second reads a
  // stored row plus the in-memory FFmpeg snapshot, so it never touches media files.
  const playbackSessionId = playbackSession?.sessionId ?? null;
  const infoStats = useQuery({
    queryKey: queryKeys.webPlaybackStats(playbackSessionId ?? ""),
    queryFn: ({ signal }) => api.webPlaybackStats(playbackSessionId ?? "", signal),
    enabled: showInfo && Boolean(playbackSessionId),
    retry: false,
    refetchInterval: showInfo ? 1_000 : false,
    staleTime: 0,
  });
  const remoteHttpSource = Boolean(source && isRemoteHttpStrmSource(source));
  const remoteDirectUrl = remoteHttpSource ? source?.externalUrl ?? null : null;
  // The remembered kind is read once per plan so recording a success below cannot reorder a ladder
  // that is already being walked.
  const candidates = useMemo(
    () => orderCandidates(directCandidates(playbackPlan, source), rememberedCandidate()),
    [playbackPlan, source],
  );
  const candidate = candidates[Math.min(candidateOffset, candidates.length - 1)];
  const hasNextCandidate = candidateOffset + 1 < candidates.length;
  const streamUrl = playbackPlan?.type === "DIRECT"
    ? candidate?.url ?? playbackPlan.url
    : playbackPlan?.type === "SERVER_HLS"
      ? playbackPlan.manifestUrl
      : "";
  const remoteCaptionRequested = remoteHttpSource
    && Boolean(
      selectedCaptionOption
      && selectedCaptionOption.renderMode === "runtime-overlay",
  );
  const clientMkvSourceUrl = playbackPlan?.type === "DIRECT"
    ? remoteDirectUrl ?? playbackPlan.rangeUrl ?? null
    : null;
  const remoteCaptionSidecarRequested = remoteCaptionRequested
    && !clientEngineActive
    && canUseRemoteMkvCaptionSidecar(source)
    && Boolean(clientMkvSourceUrl);
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
  const hasRestoredPositionRef = useRef(false);
  const splashTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const screenshotStatusTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const progressBarRef = useRef<HTMLDivElement>(null);
  const isDraggingScrubberRef = useRef(false);
  const scrubberPointerIdRef = useRef<number | null>(null);
  const fallbackRequestedRef = useRef(false);
  const sessionTransitionRef = useRef(Promise.resolve());
  const fallbackGenerationRef = useRef(0);
  const playbackEngineHandoffRef = useRef<PlaybackEngineHandoff | null>(null);
  const airPlay = usePlayerAirPlay(airPlayVideo, playbackKey);
  const {
    reportPlayback,
    stopActiveSession,
    markStarted,
    resetReportingClock,
    resetSequence,
  } = usePlaybackReporting({
    sessionIdRef: playbackSessionIdRef,
    videoRef,
    lastVideoRef,
    sessionId: playbackSession?.sessionId ?? null,
  });

  useEffect(() => {
    if (!showInfo) {
      setClientStats(null);
      return;
    }
    const sampler = createClientStatsSampler();
    const sample = () => {
      setClientStats(sampler({
        engine: engineRef.current,
        video: videoRef.current ?? lastVideoRef.current,
      }));
    };
    sample();
    const interval = window.setInterval(sample, 1_000);
    return () => window.clearInterval(interval);
  }, [showInfo, playbackKey]);

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

  const requestServerFallback = useCallback(async (reason?: unknown) => {
    if (playbackPlan?.type === "DIRECT" && hasNextCandidate) {
      if (candidate && candidate.kind === rememberedCandidate()) clearRememberedCandidate();
      setCandidateOffset((offset) => offset + 1);
      setFailedStreamUrl(null);
      setPlaybackFailure(null);
      return;
    }
    if (remoteHttpSource) {
      setFailedStreamUrl(streamUrl || null);
      setPlaybackFailure(classifyPlayerEngineFailure(reason));
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
      resetSequence();
    }
    setPlaybackFailure(null);
    setFailedStreamUrl(null);
    setPlaybackAttempt(1);
  }, [candidate, hasNextCandidate, playbackAttempt, playbackPlan?.type, remoteHttpSource, resetSequence, stopActiveSession, streamUrl]);

  useEffect(() => {
    fallbackGenerationRef.current += 1;
    resetReportingClock();
    hasRestoredPositionRef.current = false;
    fallbackRequestedRef.current = false;
    setPlaybackAttempt(0);
    setCandidateOffset(0);
    setFailedStreamUrl(null);
    setPlaybackFailure(null);
    setFallbackLoading(false);
    setClientEngineActive(false);
    setFallbackSpeedX(null);
    currentTimeRef.current = 0;
    durationRef.current = 0;
    bufferedEndRef.current = 0;
    setCurrentTime(0);
    setDuration(0);
    setBufferedEnd(0);
  }, [itemId, requestedAudioStreamIndex, requestedSourceId]);

  useEffect(() => {
    if (sessionGateKey === playbackKey) return;
    const previousSessionId = playbackSessionIdRef.current;
    playbackSessionIdRef.current = null;
    resetSequence();
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
  }, [playbackKey, resetSequence, sessionGateKey, stopActiveSession]);

  useEffect(() => {
    if (sessionGateKey !== playbackKey) return;
    if (playbackSession?.sessionId) sessionStartedRef.current = true;
    playbackSessionIdRef.current = playbackSession?.sessionId ?? null;
    resetSequence();
  }, [playbackKey, playbackSession?.sessionId, resetSequence, sessionGateKey]);

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
    const restoreEngineHandoff = async () => {
      const handoff = playbackEngineHandoffRef.current;
      if (
        !handoff
        || handoff.playbackKey !== playbackKey
        || handoff.sourceId !== (source?.id ?? "")
      ) {
        return;
      }
      playbackEngineHandoffRef.current = null;
      if (Number.isFinite(handoff.currentTime) && handoff.currentTime >= 0) {
        currentTimeRef.current = handoff.currentTime;
        setCurrentTime(handoff.currentTime);
        activeEngine.seek(handoff.currentTime);
      }
      if (handoff.playing) {
        await activeEngine.play().catch(() => undefined);
      } else {
        activeEngine.pause();
      }
    };
    const removeRuntimeSubscription = runtime.subscribeEvents((event) => {
      if (cancelled) return;
      switch (event.type) {
        case "SOURCE_READY":
          syncSnapshot(event.snapshot, true);
          restorePlaybackPosition();
          void restoreEngineHandoff();
          break;
        case "PLAYING":
          markStarted();
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
          void reportPlayback("STOPPED", true, false, initialEngine.element, sessionId);
          void stopActiveSession(sessionId);
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
    // Only the native element can stall on an unreachable entry; the client engines read the origin
    // themselves and report their own failures.
    const ladderKind = playbackPlan?.type === "DIRECT" ? candidate?.kind : undefined;
    let metadataLoaded = false;
    const handleLoadedMetadata = () => {
      metadataLoaded = true;
      if (!cancelled && ladderKind && activeEngine.kind === "native") rememberCandidate(ladderKind);
    };
    initialEngine.element.addEventListener("loadedmetadata", handleLoadedMetadata);
    const candidateTimeout = ladderKind && hasNextCandidate
      ? window.setTimeout(() => {
        if (!cancelled && !metadataLoaded && activeEngine.kind === "native") {
          void requestServerFallback("candidate timed out");
        }
      }, DIRECT_CANDIDATE_TIMEOUT_MS)
      : undefined;
    const load = async () => {
      try {
        setClientEngineActive(false);
        if (playbackPlan?.type === "SERVER_HLS") {
          initialEngine.destroy();
          const { HlsVideoEngine } = await import("./hls-playback-engine");
          if (cancelled) return;
          activeEngine = new HlsVideoEngine(initialEngine.element);
          engineRef.current = activeEngine;
        } else {
          const useMkvFallback = isMatroskaSource(source)
            ? await shouldUseClientMkv(source, initialEngine.element)
            : false;
          const useHevcFallback =
            !useMkvFallback
            && (await shouldUseClientHevc(source, initialEngine.element));
          if (useMkvFallback || useHevcFallback) {
            setFallbackLoading(true);
            setClientEngineActive(true);
            if (cancelled) return;
            initialEngine.destroy();
            if (useMkvFallback) {
              const { ClientMkvEngine } = await import("./mkv-playback-engine");
              if (cancelled) return;
              const inputCodec = source?.streams?.find((stream) => (stream.type ?? "").toUpperCase() === "VIDEO")?.codec ?? "";
              activeEngine = new ClientMkvEngine(initialEngine.element, HEVC_RUNTIME_ASSETS, inputCodec);
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
        const engineSource = useClientEngine(activeEngine)
          ? clientMkvSourceUrl ?? streamUrl
          : streamUrl;
        await runtime.load(
          new LegacyPlaybackEngineAdapter(
            activeEngine,
            playbackPlan?.type === "SERVER_HLS" ? "hls" : undefined,
          ),
          {
            id: source?.id ?? "",
            url: engineSource,
            poster,
          },
        );
        if (!cancelled) await restoreEngineHandoff();
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
      window.clearTimeout(candidateTimeout);
      initialEngine.element.removeEventListener("loadedmetadata", handleLoadedMetadata);
      if ((remoteHttpSource || candidates.length > 1) && source?.id) {
        const video = activeEngine.element;
        const currentTime = Number.isFinite(video.currentTime)
          ? Math.max(0, video.currentTime)
          : currentTimeRef.current;
        if (
          playbackEngineHandoffRef.current?.playbackKey !== playbackKey
          || playbackEngineHandoffRef.current?.sourceId !== source.id
        ) {
          playbackEngineHandoffRef.current = {
            playbackKey,
            sourceId: source.id,
            currentTime,
            playing: !video.paused && !video.ended,
          };
        }
      }
      timelineScheduler.dispose();
      initialEngine.element.removeEventListener("durationchange", handleDurationChange);
      performanceElement?.removeEventListener(PLAYBACK_PERFORMANCE_EVENT, handlePerformance);
      removeRuntimeSubscription();
      runtime.destroy();
      if (runtimeRef.current === runtime) runtimeRef.current = null;
      if (engineRef.current === activeEngine) engineRef.current = null;
    };
  }, [candidate?.kind, candidates.length, hasNextCandidate, playbackKey, playbackPlan?.type, poster, remoteHttpSource, requestServerFallback, source, streamUrl]);

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

  useRuntimeCaptionReader({
    clientEngineActive,
    clientMkvSourceUrl,
    remoteCaptionSidecarRequested,
    playbackKey,
    currentTime,
    selectedCaptionOption,
    videoRef,
    currentTimeRef,
    onCue: handleRuntimeCaptionCue,
    onStatusChange: setCaptionStatus,
    clearCues: clearRuntimeCaptionCues,
  });

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
        case "i":
        case "I":
          e.preventDefault();
          setShowInfo((visible) => !visible);
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
          if (showInfo) {
            e.preventDefault();
            setShowInfo(false);
          } else if (showSettings) {
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
    showInfo,
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
  const infoGroups = showInfo
    ? buildPlayerInfoGroups({
      plan: playbackPlan?.type ?? null,
      tier: playbackSession?.tier ?? null,
      source: summarizeSource(source),
      server: infoStats.data ?? null,
      serverPending: infoStats.isPending,
      serverError: infoStats.error instanceof Error ? infoStats.error.message : null,
      client: clientStats,
      playback: {
        positionSeconds: currentTime,
        durationSeconds: duration,
        rate: playbackRate,
        volume,
        muted: isMuted,
        paused: !playing,
      },
    })
    : [];
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
      onContextMenu={(event) => {
        const target = event.target as HTMLElement | null;
        // Text inputs keep their own context menu (paste).
        if (target?.closest("input, textarea, select, [contenteditable='true']")) return;
        event.preventDefault();
        setShowInfo((visible) => !visible);
      }}
    >
      <PlayerVideoSurface
        streamUrl={streamUrl}
        deferNativeSource={isMatroskaSource(source) && !remoteHttpSource}
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
        onRuntimeCaptionCue={handleRuntimeCaptionCue}
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
        runtimeCues={runtimeCaptionOverlayCues}
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
          selectedCaptionId={captionSourceId === source?.id ? selectedCaptionId : null}
          captionStatus={captionStatus}
          onSelectCaption={selectCaption}
          captionOffset={captionOffset}
          onChangeCaptionOffset={changeCaptionOffset}
          onClose={() => setShowSettings(false)}
        />
      ) : null}

      {showInfo ? (
        <PlayerInfoPanel
          groups={infoGroups}
          copyText={playerInfoCopyText(infoGroups)}
          onClose={() => setShowInfo(false)}
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
        infoOpen={showInfo}
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
        onToggleInfo={() => setShowInfo((visible) => !visible)}
        onTogglePictureInPicture={togglePictureInPicture}
        onToggleFullscreen={toggleFullscreen}
      />
    </LuxPlayer>
  );
}
