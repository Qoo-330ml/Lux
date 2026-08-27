import {
  Maximize,
  Minimize,
  Pause,
  PictureInPicture2,
  Play,
  RotateCcw,
  RotateCw,
  Volume1,
  Volume2,
  VolumeX,
} from "lucide-react";
import type {
  ChangeEvent,
  KeyboardEvent as ReactKeyboardEvent,
  MouseEvent as ReactMouseEvent,
  PointerEvent as ReactPointerEvent,
  RefObject,
} from "react";

export type PlayerControlsProps = {
  playing: boolean;
  currentTime: number;
  duration: number;
  bufferedEnd: number;
  volume: number;
  muted: boolean;
  playbackRate: number;
  fullscreen: boolean;
  pictureInPictureEnabled: boolean;
  remainingTime: boolean;
  hoverTime: number | null;
  hoverPercent: number | null;
  progressBarRef: RefObject<HTMLDivElement | null>;
  onTimelinePointerDown: (event: ReactPointerEvent<HTMLDivElement>) => void;
  onTimelinePointerMove: (event: ReactPointerEvent<HTMLDivElement>) => void;
  onTimelinePointerUp: (event: ReactPointerEvent<HTMLDivElement>) => void;
  onTimelinePointerCancel: (event: ReactPointerEvent<HTMLDivElement>) => void;
  onTimelineMouseMove: (event: ReactMouseEvent<HTMLDivElement>) => void;
  onTimelineMouseLeave: () => void;
  onTimelineKeyDown: (event: ReactKeyboardEvent<HTMLDivElement>) => void;
  onTogglePlayPause: () => void;
  onSeekRelative: (seconds: number) => void;
  onToggleMute: () => void;
  onVolumeChange: (volume: number) => void;
  onToggleRemainingTime: () => void;
  onCycleRate: () => void;
  onTogglePictureInPicture: () => void;
  onToggleFullscreen: () => void;
};

export function formatTime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return "00:00";
  const totalSeconds = Math.floor(seconds);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const remainder = totalSeconds % 60;
  if (hours > 0) {
    return `${hours}:${String(minutes).padStart(2, "0")}:${String(remainder).padStart(2, "0")}`;
  }
  return `${String(minutes).padStart(2, "0")}:${String(remainder).padStart(2, "0")}`;
}

export function PlayerControls({
  playing,
  currentTime,
  duration,
  bufferedEnd,
  volume,
  muted,
  playbackRate,
  fullscreen,
  pictureInPictureEnabled,
  remainingTime,
  hoverTime,
  hoverPercent,
  progressBarRef,
  onTimelinePointerDown,
  onTimelinePointerMove,
  onTimelinePointerUp,
  onTimelinePointerCancel,
  onTimelineMouseMove,
  onTimelineMouseLeave,
  onTimelineKeyDown,
  onTogglePlayPause,
  onSeekRelative,
  onToggleMute,
  onVolumeChange,
  onToggleRemainingTime,
  onCycleRate,
  onTogglePictureInPicture,
  onToggleFullscreen,
}: PlayerControlsProps) {
  const progressPercent = duration > 0 ? (currentTime / duration) * 100 : 0;
  const bufferedPercent = duration > 0 ? (bufferedEnd / duration) * 100 : 0;

  return (
    <div className="lux-player-controls-wrap">
      <div
        ref={progressBarRef}
        className="lux-player-timeline"
        role="slider"
        tabIndex={0}
        aria-label="播放进度"
        aria-valuemin={0}
        aria-valuemax={Math.max(0, Math.round(duration))}
        aria-valuenow={Math.max(0, Math.round(currentTime))}
        aria-valuetext={`${formatTime(currentTime)} / ${formatTime(duration)}`}
        onPointerDown={onTimelinePointerDown}
        onPointerMove={onTimelinePointerMove}
        onPointerUp={onTimelinePointerUp}
        onPointerCancel={onTimelinePointerCancel}
        onMouseMove={onTimelineMouseMove}
        onMouseLeave={onTimelineMouseLeave}
        onKeyDown={onTimelineKeyDown}
      >
        {hoverTime !== null && hoverPercent !== null ? (
          <div className="lux-player-tooltip" style={{ left: `${hoverPercent}%` }} aria-hidden="true">
            {formatTime(hoverTime)}
          </div>
        ) : null}

        <div className="lux-player-timeline-rail" aria-hidden="true">
          <div className="lux-player-timeline-buffered" style={{ width: `${bufferedPercent}%` }} />
          <div className="lux-player-timeline-played" style={{ width: `${progressPercent}%` }}>
            <div className="lux-player-timeline-handle" />
          </div>
        </div>
      </div>

      <div className="lux-player-controls">
        <div className="lux-player-controls-left">
          <button
            type="button"
            className="lux-player-action-btn lux-player-play-btn"
            aria-label={playing ? "暂停" : "播放"}
            title={playing ? "暂停 (空格)" : "播放 (空格)"}
            onClick={onTogglePlayPause}
          >
            {playing ? <Pause size={22} fill="currentColor" aria-hidden="true" /> : <Play size={22} fill="currentColor" aria-hidden="true" />}
          </button>

          <button type="button" className="lux-player-action-btn" aria-label="快退10秒" title="快退 10 秒 (←)" onClick={() => onSeekRelative(-10)}>
            <RotateCcw size={19} aria-hidden="true" />
            <span className="lux-player-step-label">10</span>
          </button>
          <button type="button" className="lux-player-action-btn" aria-label="快进10秒" title="快进 10 秒 (→)" onClick={() => onSeekRelative(10)}>
            <RotateCw size={19} aria-hidden="true" />
            <span className="lux-player-step-label">10</span>
          </button>

          <div className="lux-player-volume-group">
            <button
              type="button"
              className="lux-player-action-btn"
              aria-label={muted ? "取消静音" : "静音"}
              title={muted ? "取消静音 (M)" : "静音 (M)"}
              onClick={onToggleMute}
            >
              {muted || volume === 0 ? <VolumeX size={20} aria-hidden="true" /> : volume < 0.5 ? <Volume1 size={20} aria-hidden="true" /> : <Volume2 size={20} aria-hidden="true" />}
            </button>
            <div className="lux-player-volume-slider-wrap">
              <input
                type="range"
                min={0}
                max={1}
                step={0.02}
                value={muted ? 0 : volume}
                onChange={(event: ChangeEvent<HTMLInputElement>) => onVolumeChange(parseFloat(event.target.value))}
                className="lux-player-volume-slider"
                aria-label="音量调节"
              />
            </div>
          </div>

          <button
            type="button"
            className="lux-player-time"
            onClick={onToggleRemainingTime}
            title="点击切换显示剩余时间"
            aria-label="切换剩余时间显示"
          >
            <span className="lux-player-time-current">{formatTime(currentTime)}</span>
            <span className="lux-player-time-divider" aria-hidden="true">/</span>
            <span className="lux-player-time-total">
              {remainingTime ? `-${formatTime(Math.max(0, duration - currentTime))}` : formatTime(duration)}
            </span>
          </button>
        </div>

        <div className="lux-player-controls-right">
          <button type="button" className="lux-player-rate-btn" aria-label="倍速切换" title="切换倍速" onClick={onCycleRate}>
            {playbackRate === 1 ? "倍速" : `${playbackRate}x`}
          </button>
          {pictureInPictureEnabled ? (
            <button type="button" className="lux-player-action-btn" aria-label="画中画" title="画中画" onClick={onTogglePictureInPicture}>
              <PictureInPicture2 size={19} aria-hidden="true" />
            </button>
          ) : null}
          <button type="button" className="lux-player-action-btn" aria-label={fullscreen ? "退出全屏" : "全屏"} title={fullscreen ? "退出全屏 (F)" : "全屏 (F)"} onClick={onToggleFullscreen}>
            {fullscreen ? <Minimize size={20} aria-hidden="true" /> : <Maximize size={20} aria-hidden="true" />}
          </button>
        </div>
      </div>
    </div>
  );
}
