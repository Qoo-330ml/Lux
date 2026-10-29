import "./player.css";
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useNavigate, useParams, useSearchParams } from "react-router-dom";
import { ApiError } from "../../lib/api/client";
import { imageUrl, mediaTitle } from "../home/media";
import { usePlayerPlaybackControls } from "./usePlayerPlaybackControls";
import { usePlayerTimelineScrubber } from "./usePlayerTimelineScrubber";
import { usePlayerKeyboardShortcuts } from "./usePlayerKeyboardShortcuts";
import { usePlayerScreenshot } from "./usePlayerScreenshot";
import { usePlayerInfoPanel } from "./usePlayerInfoPanel";
import { usePlayerPlaybackSession } from "./usePlayerPlaybackSession";
import { usePlayerVideoElement } from "./usePlayerVideoElement";
import { usePlayerNavigation } from "./usePlayerNavigation";
import { usePlaybackSessionGate } from "./usePlaybackSessionGate";
import { usePlayerControlsVisibility } from "./usePlayerControlsVisibility";
import type { PlaybackEngine } from "./playback-engine";
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
import { playerInfoCopyText } from "./player-stats";
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
import { usePlaybackReporting } from "./usePlaybackReporting";
import { useCaptionSelection } from "./useCaptionSelection";
import { useRuntimeCaptionReader } from "./useRuntimeCaptionReader";
import { usePlaybackRuntime } from "./usePlaybackRuntime";
import {
  getSubtitleInfo,
  isMatroskaSource,
} from "./player-page-helpers";

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
  const [isRemainingTime, setIsRemainingTime] = useState(false);
  const [isPointerInteracting, setIsPointerInteracting] = useState(false);
  const [danmuVisible, setDanmuVisible] = useState(true);
  const [airPlayVideo, setAirPlayVideo] = useState<HTMLVideoElement | null>(null);
  const currentTimeRef = useRef(0);
  const durationRef = useRef(0);
  const bufferedEndRef = useRef(0);
  const engineRef = useRef<PlaybackEngine | null>(null);

  const requestedSourceId = searchParams.get("sourceId");
  const requestedAudioStreamIndex = searchParams.get("audioStreamIndex");
  const requestedSubtitleStreamIndex = searchParams.get("subtitleStreamIndex");
  const {
    playbackBootstrap,
    item,
    webPlaybackSession,
    playbackDataRef,
    media,
    source,
    episodeNavigation,
    playbackSession,
    playbackPlan,
    playbackSessionId,
    playbackKey,
    sessionGateKey,
    setSessionGateKey,
    sessionStartedRef,
    playbackSessionIdRef,
    useLegacyPlaybackQueries,
  } = usePlayerPlaybackSession({
    itemId,
    requestedSourceId,
    requestedAudioStreamIndex,
    requestedSubtitleStreamIndex,
    playbackAttempt,
  });

  const nativeCaptionTracksSupported = typeof HTMLTrackElement !== "undefined";
  const {
    controlsVisible,
    setControlsVisible,
    setControlActivity,
    resetControlsTimeout,
  } = usePlayerControlsVisibility({ playing, showSettings, isPointerInteracting });
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
    engineRef,
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
  const runtimeRef = useRef<LuxPlayerRuntime | null>(null);
  const hasRestoredPositionRef = useRef(false);
  const progressBarRef = useRef<HTMLDivElement>(null);
  const fallbackRequestedRef = useRef(false);
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

  const { infoGroups } = usePlayerInfoPanel({
    playbackSessionId,
    showInfo,
    playbackKey,
    playbackPlan,
    tier: playbackSession?.tier ?? null,
    source,
    engineRef,
    videoRef,
    lastVideoRef,
    currentTime,
    duration,
    playbackRate,
    volume,
    isMuted,
    playing,
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

  const setVideoRef = usePlayerVideoElement({
    videoRef,
    lastVideoRef,
    engineRef,
    runtimeRef,
    setAirPlayVideo,
    setNativeCaptionTracks,
  });

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

  usePlaybackSessionGate({
    playbackKey,
    sessionGateKey,
    setSessionGateKey,
    playbackSession,
    playbackSessionIdRef,
    sessionStartedRef,
    stopActiveSession,
    resetSequence,
  });

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

  const { screenshotStatus, takeScreenshot } = usePlayerScreenshot({ videoRef, media });

  const { handleBack, navigateToEpisode } = usePlayerNavigation({
    itemId,
    navigate: (to) => navigate(to as never),
    setShowSettings,
  });

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
