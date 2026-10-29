import "./player.css";
import { useQuery } from "@tanstack/react-query";
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useNavigate, useParams, useSearchParams } from "react-router-dom";
import { ApiError, api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { WebPlaybackTrackSelection } from "../../lib/api/types";
import { imageUrl, mediaTitle } from "../home/media";
import { usePlayerPlaybackControls } from "./usePlayerPlaybackControls";
import { usePlayerTimelineScrubber } from "./usePlayerTimelineScrubber";
import { usePlayerKeyboardShortcuts } from "./usePlayerKeyboardShortcuts";
import { NativeVideoEngine, type PlaybackEngine } from "./playback-engine";
import {
  clearRememberedCandidate,
  directCandidates,
  orderCandidates,
  rememberedCandidate,
} from "./direct-candidates";
import { canUseRemoteMkvCaptionSidecar, isRemoteHttpStrmSource } from "./playback-selection";
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
import { usePlaybackRuntime } from "./usePlaybackRuntime";
import {
  episodeNavigationFor,
  getSubtitleInfo,
  isMatroskaSource,
  screenshotFileName,
} from "./player-page-helpers";

const AUTO_HIDE_DELAY_MS = 3_000;
const PLAYBACK_SPEEDS = [0.5, 0.75, 1.0, 1.25, 1.5, 2.0];

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
  const [loopPlayback, setLoopPlayback] = useState(false);
  const [aspectRatio, setAspectRatio] = useState<PlayerAspectRatio>("default");
  const [flip, setFlip] = useState<PlayerFlip>("normal");
  const [showSettings, setShowSettings] = useState(false);
  const [showInfo, setShowInfo] = useState(false);
  const [clientStats, setClientStats] = useState<PlayerClientStats | null>(null);
  const [isRemainingTime, setIsRemainingTime] = useState(false);
  const [controlsVisible, setControlsVisible] = useState(true);
  const [isPointerInteracting, setIsPointerInteracting] = useState(false);
  const [controlActivity, setControlActivity] = useState(0);
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
  const screenshotStatusTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const progressBarRef = useRef<HTMLDivElement>(null);
  const fallbackRequestedRef = useRef(false);
  const sessionTransitionRef = useRef(Promise.resolve());
  const fallbackGenerationRef = useRef(0);
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

  const {
    volume,
    isMuted,
    playbackRate,
    isFullscreen,
    centerSplash,
    togglePlayPause,
    seekTo,
    seekRelative,
    toggleMute,
    changeVolume,
    changePlaybackRate,
    toggleFullscreen,
    togglePictureInPicture,
  } = usePlayerPlaybackControls({
    videoRef,
    runtimeRef,
    playerContainerRef,
    duration,
    setCurrentTime,
  });

  const {
    hoverTime,
    hoverPercent,
    isDraggingScrubberRef,
    handleScrubberPointerDown,
    handleScrubberPointerMove,
    finishScrubberPointer,
    handleScrubberMouseMove,
    handleScrubberMouseLeave,
    handleTimelineKeyDown,
  } = usePlayerTimelineScrubber({
    progressBarRef,
    duration,
    seekTo,
    seekRelative,
    setIsPointerInteracting,
    resetControlsTimeout,
  });

  usePlayerKeyboardShortcuts({
    itemId,
    volume,
    changeVolume,
    seekRelative,
    togglePlayPause,
    toggleMute,
    toggleFullscreen,
    resetControlsTimeout,
    showInfo,
    showSettings,
    setShowInfo,
    setShowSettings,
    navigate: (to) => navigate(to as never),
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

  usePlaybackRuntime({
    playbackKey,
    source,
    playbackPlan,
    streamUrl,
    poster,
    candidate,
    candidates,
    hasNextCandidate,
    remoteHttpSource,
    clientMkvSourceUrl,
    playbackDataRef,
    hasRestoredPositionRef,
    videoRef,
    engineRef,
    runtimeRef,
    currentTimeRef,
    durationRef,
    bufferedEndRef,
    isDraggingScrubberRef,
    playbackSessionIdRef,
    reportPlayback,
    stopActiveSession,
    markStarted,
    requestServerFallback: requestServerFallback as (reason?: unknown) => Promise<void>,
    setCurrentTime,
    setDuration,
    setBufferedEnd,
    setPlaying,
    setControlsVisible,
    setControlActivity,
    setFailedStreamUrl,
    setPlaybackFailure,
    setFallbackLoading,
    setFallbackSpeedX,
    setClientEngineActive,
  });

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

  useEffect(() => {
    if (!playing || !controlsVisible || showSettings || isPointerInteracting) return;
    const timeout = window.setTimeout(() => setControlsVisible(false), AUTO_HIDE_DELAY_MS);
    return () => window.clearTimeout(timeout);
  }, [controlActivity, controlsVisible, isPointerInteracting, playing, showSettings]);

  useEffect(() => () => {
    if (screenshotStatusTimeoutRef.current) clearTimeout(screenshotStatusTimeoutRef.current);
  }, []);

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
