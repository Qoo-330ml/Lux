import { AlertCircle, ArrowLeft, Pause, Play } from "lucide-react";
import type {
  MouseEvent as ReactMouseEvent,
  RefCallback,
  SyntheticEvent,
} from "react";

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
  centerSplash: "play" | "pause" | null;
  fallbackLoading: boolean;
  fallbackSpeedX: number | null;
  errorMessage: string | null;
  showError: boolean;
  onRetry: () => void;
  onBack: () => void;
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
  centerSplash,
  fallbackLoading,
  fallbackSpeedX,
  errorMessage,
  showError,
  onRetry,
  onBack,
}: PlayerVideoSurfaceProps) {
  return (
    <div className="lux-player-frame">
      {streamUrl ? (
        <video
          ref={videoRef}
          className="lux-video"
          src={streamUrl}
          poster={poster ?? undefined}
          preload="metadata"
          onClick={onClick}
          onDoubleClick={onDoubleClick}
          onError={onError}
          onLoadedMetadata={onLoadedMetadata}
          onPlay={onPlay}
          onPause={onPause}
          onTimeUpdate={onTimeUpdate}
          onEnded={onEnded}
          aria-label={`播放 ${title}`}
        />
      ) : null}

      {centerSplash ? (
        <div className="lux-player-center-splash" aria-hidden="true">
          {centerSplash === "play" ? (
            <Play size={48} fill="currentColor" />
          ) : (
            <Pause size={48} fill="currentColor" />
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
            <p className="lux-player-error">
              {errorMessage ?? "浏览器无法播放这个媒体源。请尝试其他版本或使用支持该格式的客户端。"}
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
