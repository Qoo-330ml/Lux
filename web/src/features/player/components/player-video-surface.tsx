import { AlertCircle, ArrowLeft, Pause, Play } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type {
  MouseEvent as ReactMouseEvent,
  PointerEvent as ReactPointerEvent,
  RefCallback,
  SyntheticEvent,
} from "react";
import type { PlayerFailure } from "./player-diagnostics";
import { usePlayerSurfaceGestures } from "./player-gestures";
import type { PlayerNativeCaptionTrack } from "./player-captions";
import { createNativeCaptionOffsetController } from "../caption-offset";
import {
  DEFAULT_VIDEO_PRESENTATION,
  playerVideoPresentationSize,
  playerVideoPresentationStyle,
  type PlayerVideoPresentation,
} from "./player-presentation";

type PlayerVideoSurfaceProps = {
  streamUrl: string;
  poster?: string | null;
  title: string;
  videoRef: RefCallback<HTMLVideoElement>;
  onClick: (event: ReactMouseEvent<HTMLVideoElement>) => void;
  onDoubleClick: (event: ReactMouseEvent<HTMLVideoElement>) => void;
  onError?: (event: SyntheticEvent<HTMLVideoElement>) => void;
  onLoadedMetadata?: () => void;
  onPlay?: () => void;
  onPause?: (event: SyntheticEvent<HTMLVideoElement>) => void;
  onTimeUpdate?: () => void;
  onEnded?: () => void;
  captionTrack?: PlayerNativeCaptionTrack | null;
  captionOffset?: number;
  captionDuration?: number | null;
  captionLifecycleKey?: string;
  onCaptionTrackLoad?: () => void;
  onCaptionTrackError?: () => void;
  presentation?: PlayerVideoPresentation;
  playing?: boolean;
  onTogglePlayback?: () => void;
  centerSplash: "play" | "pause" | null;
  fallbackLoading: boolean;
  fallbackSpeedX: number | null;
  errorMessage: string | null;
  failure?: PlayerFailure | null;
  showError: boolean;
  onRetry: () => void;
  onBack: () => void;
  gestureOptions?: {
    currentTime: number;
    duration: number;
    volume: number;
    onSeekTo: (position: number) => void;
    onVolumeChange: (volume: number) => void;
    onSeekRelative: (seconds: number) => void;
    onSingleTap: () => void;
    onActivity: () => void;
    onInteractionChange: (interacting: boolean) => void;
  };
};

export function PlayerVideoSurface({
  streamUrl,
  poster,
  title,
  videoRef,
  onClick,
  onDoubleClick,
  onError,
  onLoadedMetadata,
  onPlay,
  onPause,
  onTimeUpdate,
  onEnded,
  captionTrack = null,
  captionOffset = 0,
  captionDuration = null,
  captionLifecycleKey = "",
  onCaptionTrackLoad,
  onCaptionTrackError,
  presentation = DEFAULT_VIDEO_PRESENTATION,
  playing = false,
  onTogglePlayback = () => undefined,
  centerSplash,
  fallbackLoading,
  fallbackSpeedX,
  errorMessage,
  failure,
  showError,
  onRetry,
  onBack,
  gestureOptions,
}: PlayerVideoSurfaceProps) {
  const frameRef = useRef<HTMLDivElement>(null);
  const captionTrackRef = useRef<HTMLTrackElement>(null);
  const captionOffsetControllerRef = useRef<ReturnType<typeof createNativeCaptionOffsetController> | null>(null);
  const [presentationSize, setPresentationSize] = useState<ReturnType<typeof playerVideoPresentationSize>>(null);
  const gestures = usePlayerSurfaceGestures({
    enabled: Boolean(gestureOptions),
    currentTime: gestureOptions?.currentTime ?? 0,
    duration: gestureOptions?.duration ?? 0,
    volume: gestureOptions?.volume ?? 0,
    onSeekTo: gestureOptions?.onSeekTo ?? (() => undefined),
    onVolumeChange: gestureOptions?.onVolumeChange ?? (() => undefined),
    onSeekRelative: gestureOptions?.onSeekRelative ?? (() => undefined),
    onSingleTap: gestureOptions?.onSingleTap ?? (() => undefined),
    onActivity: gestureOptions?.onActivity ?? (() => undefined),
    onInteractionChange: gestureOptions?.onInteractionChange ?? (() => undefined),
  });

  useEffect(() => {
    if (presentation.aspectRatio === "default") {
      setPresentationSize(null);
      return;
    }
    const frame = frameRef.current;
    if (!frame) return;
    const updateSize = () => {
      const nextSize = playerVideoPresentationSize(
        presentation.aspectRatio,
        frame.clientWidth,
        frame.clientHeight,
      );
      setPresentationSize((previous) => (
        previous?.width === nextSize?.width && previous?.height === nextSize?.height
          ? previous
          : nextSize
      ));
    };
    updateSize();
    if (typeof ResizeObserver !== "undefined") {
      const observer = new ResizeObserver(updateSize);
      observer.observe(frame);
      return () => observer.disconnect();
    }
    window.addEventListener("resize", updateSize);
    return () => window.removeEventListener("resize", updateSize);
  }, [presentation.aspectRatio]);

  const handleClick = (event: ReactMouseEvent<HTMLVideoElement>) => {
    if (gestures.consumeSuppressedClick(event)) return;
    onClick(event);
  };

  const handlePointerDown = (event: ReactPointerEvent<HTMLVideoElement>) => {
    gestures.onPointerDown(event);
  };

  const handleDoubleClick = (event: ReactMouseEvent<HTMLVideoElement>) => {
    if (gestures.consumeSuppressedClick(event)) return;
    onDoubleClick(event);
  };

  useEffect(() => {
    const track = captionTrackRef.current;
    const nativeTrack = track?.track;
    if (!nativeTrack || !captionTrack) return;
    const controller = createNativeCaptionOffsetController(nativeTrack);
    captionOffsetControllerRef.current = controller;
    nativeTrack.mode = "showing";
    controller.apply(captionOffset, captionDuration);
    return () => {
      controller.restore();
      nativeTrack.mode = "disabled";
      if (captionOffsetControllerRef.current === controller) {
        captionOffsetControllerRef.current = null;
      }
    };
  }, [captionLifecycleKey, captionTrack?.id, captionTrack?.src]);

  useEffect(() => {
    captionOffsetControllerRef.current?.apply(captionOffset, captionDuration);
  }, [captionDuration, captionOffset]);

  const handleCaptionTrackLoad = () => {
    captionOffsetControllerRef.current?.apply(captionOffset, captionDuration);
    onCaptionTrackLoad?.();
  };

  return (
    <div ref={frameRef} className="lux-player-frame">
      {streamUrl ? (
        <video
          ref={videoRef}
          className="lux-video"
          src={streamUrl}
          poster={poster ?? undefined}
          preload="metadata"
          loop={presentation.loop}
          style={playerVideoPresentationStyle(presentation.aspectRatio, presentation.flip, presentationSize)}
          onClick={handleClick}
          onDoubleClick={handleDoubleClick}
          onPointerDown={handlePointerDown}
          onPointerMove={gestures.onPointerMove}
          onPointerUp={gestures.onPointerUp}
          onPointerCancel={gestures.onPointerCancel}
          onError={onError}
          onLoadedMetadata={onLoadedMetadata}
          onPlay={onPlay}
          onPause={onPause}
          onTimeUpdate={onTimeUpdate}
          onEnded={onEnded}
          aria-label={`播放 ${title}`}
        >
          {captionTrack ? (
            <track
              key={`${captionTrack.src}:${captionLifecycleKey}`}
              ref={captionTrackRef}
              kind="subtitles"
              label={captionTrack.label}
              srcLang={captionTrack.language}
              src={captionTrack.src}
              onLoad={handleCaptionTrackLoad}
              onError={onCaptionTrackError}
            />
          ) : null}
        </video>
      ) : null}

      {!playing && streamUrl && !fallbackLoading && !showError ? (
        <button
          type="button"
          className="lux-player-center-play"
          aria-label="播放"
          title="播放 (空格)"
          onClick={onTogglePlayback}
        >
          <Play size={34} fill="currentColor" aria-hidden="true" />
        </button>
      ) : null}

      {centerSplash ? (
        <div className="lux-player-center-splash" aria-hidden="true">
          {centerSplash === "play" ? (
            <Play size={38} fill="currentColor" />
          ) : (
            <Pause size={38} fill="currentColor" />
          )}
        </div>
      ) : null}

      {fallbackLoading ? (
        <div className="lux-player-status-badge" role="status">
          <span className="lux-player-status">正在准备客户端解码…</span>
        </div>
      ) : null}

      {fallbackSpeedX !== null ? (
        <div className="lux-player-speed-alert" role="status">
          <p className="lux-player-status">
            客户端解码速度低于实时（约 {fallbackSpeedX.toFixed(2)}×），当前已缓存后播放；建议使用原生客户端或降低清晰度。
          </p>
        </div>
      ) : null}

      {showError ? (
        <div className="lux-player-error-modal" role="alert">
          <div className="lux-player-error-card">
            <AlertCircle size={36} className="lux-player-error-icon" aria-hidden="true" />
            {failure ? <h2>{failure.title}</h2> : null}
            <p className="lux-player-error">
              {failure?.message ?? errorMessage ?? "浏览器无法播放这个媒体源。请尝试其他版本或使用支持该格式的客户端。"}
            </p>
            <div className="lux-player-error-actions">
              <button
                className="lux-player-glass-btn"
                type="button"
                onClick={onRetry}
                aria-label="重试"
              >
                重试
              </button>
              <button
                className="lux-player-glass-btn lux-player-glass-btn-primary"
                type="button"
                onClick={onBack}
                aria-label="返回上一页"
              >
                <ArrowLeft size={16} aria-hidden="true" /> 返回
              </button>
            </div>
          </div>
        </div>
      ) : null}
    </div>
  );
}
