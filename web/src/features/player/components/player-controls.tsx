import {
  Camera,
  Airplay,
  Maximize,
  MessageCircleMore,
  Minimize,
  Pause,
  PictureInPicture2,
  Play,
  Settings2,
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
import { PlayerChapterTimeline, PlayerIntroSkip } from "./player-chapter-timeline";
import type { PlayerChapterSegment, PlayerIntroRange } from "../player-chapters";

export type PlayerControlsProps = {
  playing: boolean;
  currentTime: number;
  duration: number;
  bufferedEnd: number;
  volume: number;
  muted: boolean;
  fullscreen: boolean;
  pictureInPictureEnabled: boolean;
  sources: readonly PlayerControlSourceOption[];
  selectedSourceId: string;
  danmuVisible: boolean;
  airPlayAvailable?: boolean;
  chapters?: readonly PlayerChapterSegment[];
  introSkip?: PlayerIntroRange | null;
  settingsOpen: boolean;
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
  onToggleMute: () => void;
  onVolumeChange: (volume: number) => void;
  onToggleRemainingTime: () => void;
  onSourceChange: (sourceId: string) => void;
  onToggleDanmu: () => void;
  onAirPlay?: () => void;
  onChapterSeek?: (seconds: number) => void;
  onSkipIntro?: (seconds: number) => void;
  onTakeScreenshot: () => void;
  onToggleSettings: () => void;
  onTogglePictureInPicture: () => void;
  onToggleFullscreen: () => void;
};

export type PlayerControlSourceOption = {
  id: string;
  label: string;
  detail: string;
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
  fullscreen,
  pictureInPictureEnabled,
  sources,
  selectedSourceId,
  danmuVisible,
  airPlayAvailable = false,
  chapters = [],
  introSkip = null,
  settingsOpen,
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
  onToggleMute,
  onVolumeChange,
  onToggleRemainingTime,
  onSourceChange,
  onToggleDanmu,
  onAirPlay = () => undefined,
  onChapterSeek = () => undefined,
  onSkipIntro = () => undefined,
  onTakeScreenshot,
  onToggleSettings,
  onTogglePictureInPicture,
  onToggleFullscreen,
}: PlayerControlsProps) {
  const progressPercent = duration > 0 ? (currentTime / duration) * 100 : 0;
  const bufferedPercent = duration > 0 ? (bufferedEnd / duration) * 100 : 0;

  return (
    <div className="lux-player-controls-wrap">
      <PlayerIntroSkip currentTime={currentTime} introSkip={introSkip} onSkip={onSkipIntro} />
      <PlayerChapterTimeline segments={chapters} duration={duration} onSeek={onChapterSeek} />
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

        <div className="lux-player-controls-middle">
          <button
            type="button"
            className={`lux-player-action-btn lux-player-danmu-btn ${danmuVisible ? "is-active" : ""}`}
            aria-label={danmuVisible ? "隐藏弹幕" : "显示弹幕"}
            aria-pressed={danmuVisible}
            title={danmuVisible ? "隐藏弹幕" : "显示弹幕"}
            onClick={onToggleDanmu}
          >
            <MessageCircleMore size={20} aria-hidden="true" />
          </button>
        </div>

        <div className="lux-player-controls-right">
          {sources.length > 0 ? (
            <select
              className="lux-player-source-control"
              aria-label="选择播放版本"
              title="选择播放版本"
              value={selectedSourceId}
              onChange={(event) => onSourceChange(event.target.value)}
            >
              {sources.map((source) => (
                <option key={source.id} value={source.id}>
                  {source.label} ({source.detail})
                </option>
              ))}
            </select>
          ) : null}
          {airPlayAvailable ? (
            <button type="button" className="lux-player-action-btn" aria-label="AirPlay" title="AirPlay" onClick={onAirPlay}>
              <Airplay size={20} aria-hidden="true" />
            </button>
          ) : null}
          <button type="button" className="lux-player-action-btn" aria-label="截图" title="截图" onClick={onTakeScreenshot}>
            <Camera size={20} aria-hidden="true" />
          </button>
          <button
            type="button"
            className={`lux-player-action-btn ${settingsOpen ? "is-active" : ""}`}
            aria-label="播放器设置"
            aria-expanded={settingsOpen}
            aria-pressed={settingsOpen}
            title="设置"
            onClick={onToggleSettings}
          >
            <Settings2 size={20} aria-hidden="true" />
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
