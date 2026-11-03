import {
  Airplay,
  AudioLines,
  Gauge,
  Maximize,
  MessageCircleMore,
  Minimize,
  Pause,
  Play,
  RotateCcw,
  RotateCw,
  Settings2,
  SkipBack,
  SkipForward,
  Subtitles,
  Volume1,
  Volume2,
  VolumeX,
} from "lucide-react";
import {
  useEffect,
  useRef,
  useState,
  type ChangeEvent,
  type CSSProperties,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
  type RefObject,
} from "react";
import {
  PlayerChapterTimeline,
  PlayerIntroSkip,
  PlayerSkipCredits,
} from "./player-chapter-timeline";
import type { PlayerChapterSegment, PlayerIntroRange } from "../player-chapters";

export type PlayerControlsProps = {
  playing: boolean;
  currentTime: number;
  duration: number;
  bufferedEnd: number;
  volume: number;
  muted: boolean;
  fullscreen: boolean;
  danmuVisible: boolean;
  episodeNavigation?: PlayerEpisodeNavigation | null;
  airPlayAvailable?: boolean;
  chapters?: readonly PlayerChapterSegment[];
  introSkip?: PlayerIntroRange | null;
  creditsStart?: number | null;
  hasNextEpisode?: boolean;
  settingsOpen: boolean;
  /**
   * Which control-bar popover is open. The page owns this when it passes it, so the open popover
   * survives a remount of the control bar (for example a blocking notice swapping the surface).
   */
  trackMenu?: TrackMenu | null;
  onTrackMenuChange?: (menu: TrackMenu | null) => void;
  /** 字幕弹窗内容（选择字幕与字幕偏移）；缺省时不显示字幕按钮。 */
  subtitleMenu?: ReactNode;
  subtitleActive?: boolean;
  /** 音轨弹窗内容；缺省时不显示音轨按钮。 */
  audioMenu?: ReactNode;
  playbackRate?: number;
  playbackRates?: readonly number[];
  onChangeRate?: (rate: number) => void;
  /** 相对当前位置快退/快进（秒）；缺省时不显示 10 秒按钮。 */
  onSeekBy?: (seconds: number) => void;
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
  onNavigateToEpisode?: (episodeId: string) => void;
  onToggleMute: () => void;
  onVolumeChange: (volume: number) => void;
  onToggleRemainingTime: () => void;
  onToggleDanmu: () => void;
  onAirPlay?: () => void;
  onChapterSeek?: (seconds: number) => void;
  onSkipIntro?: (seconds: number) => void;
  onSkipCredits?: () => void;
  onToggleSettings: () => void;
  onToggleFullscreen: () => void;
};

export type PlayerEpisodeNavigation = {
  previousEpisodeId: string | null;
  nextEpisodeId: string | null;
};

export type TrackMenu = "subtitle" | "audio";

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

function rateLabel(rate: number): string {
  return rate === 1 ? "标准" : `${rate}x`;
}

export function PlayerControls({
  playing,
  currentTime,
  duration,
  bufferedEnd,
  volume,
  muted,
  fullscreen,
  danmuVisible,
  episodeNavigation = null,
  airPlayAvailable = false,
  chapters = [],
  introSkip = null,
  creditsStart = null,
  hasNextEpisode = false,
  settingsOpen,
  trackMenu,
  onTrackMenuChange,
  subtitleMenu = null,
  subtitleActive = false,
  audioMenu = null,
  playbackRate = 1,
  playbackRates = [1],
  onChangeRate = () => undefined,
  onSeekBy,
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
  onNavigateToEpisode = () => undefined,
  onToggleMute,
  onVolumeChange,
  onToggleRemainingTime,
  onToggleDanmu,
  onAirPlay = () => undefined,
  onChapterSeek = () => undefined,
  onSkipIntro = () => undefined,
  onSkipCredits = () => undefined,
  onToggleSettings,
  onToggleFullscreen,
}: PlayerControlsProps) {
  const progressPercent = duration > 0 ? (currentTime / duration) * 100 : 0;
  const bufferedPercent = duration > 0 ? (bufferedEnd / duration) * 100 : 0;
  const normalizedVolume = Number.isFinite(volume) ? Math.max(0, Math.min(1, muted ? 0 : volume)) : 0;
  const volumePercent = `${normalizedVolume * 100}%`;
  const hoverChapterTitle = hoverTime === null
    ? null
    : chapters.find((chapter) => hoverTime >= chapter.start && hoverTime < chapter.end)?.title ?? null;

  // Track popovers (subtitles, audio) open from the control bar. Only one is open at a time;
  // a pointer press outside the control bar or Escape closes it.
  const [localMenu, setLocalMenu] = useState<TrackMenu | null>(null);
  const openMenu = trackMenu !== undefined ? trackMenu : localMenu;
  const setOpenMenu = onTrackMenuChange ?? setLocalMenu;
  const wrapRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (openMenu === null) return undefined;
    const closeOnOutside = (event: PointerEvent) => {
      const target = event.target;
      if (target instanceof Node && wrapRef.current?.contains(target)) return;
      // LuxSelect renders its option list into document.body; picking an option is still inside the popover.
      if (target instanceof Element && target.closest(".lux-select-menu")) return;
      setOpenMenu(null);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpenMenu(null);
    };
    document.addEventListener("pointerdown", closeOnOutside);
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("pointerdown", closeOnOutside);
      document.removeEventListener("keydown", closeOnEscape);
    };
  }, [openMenu, setOpenMenu]);
  const toggleMenu = (menu: TrackMenu) => setOpenMenu(openMenu === menu ? null : menu);

  const cycleRate = () => {
    if (playbackRates.length === 0) return;
    const index = playbackRates.indexOf(playbackRate);
    const next = playbackRates[(index + 1) % playbackRates.length];
    if (next !== undefined) onChangeRate(next);
  };

  return (
    <div className="lux-player-controls-wrap" ref={wrapRef}>
      <PlayerIntroSkip currentTime={currentTime} introSkip={introSkip} onSkip={onSkipIntro} />
      <PlayerSkipCredits
        currentTime={currentTime}
        duration={duration}
        creditsStart={creditsStart}
        introRange={introSkip}
        hasNextEpisode={hasNextEpisode}
        onSkip={onSkipCredits}
      />
      <div className="lux-player-timeline-row">
        <div className="lux-player-timeline-times">
          <span className="lux-player-time-current">{formatTime(currentTime)}</span>
          <button
            type="button"
            className="lux-player-time-total"
            onClick={onToggleRemainingTime}
            title="点击切换显示剩余时间"
            aria-label="切换剩余时间显示"
          >
            {remainingTime ? `-${formatTime(Math.max(0, duration - currentTime))}` : formatTime(duration)}
          </button>
        </div>

        <div className="lux-player-timeline">
          {hoverTime !== null && hoverPercent !== null ? (
            <div className="lux-player-tooltip" style={{ left: `${hoverPercent}%` }} aria-hidden="true">
              {hoverChapterTitle ? `${hoverChapterTitle} · ` : ""}{formatTime(hoverTime)}
            </div>
          ) : null}

          <div className="lux-player-timeline-rail" aria-hidden="true">
            <div className="lux-player-timeline-buffered" style={{ width: `${bufferedPercent}%` }} />
            <div className="lux-player-timeline-played" style={{ width: `${progressPercent}%` }}>
              <div className="lux-player-timeline-handle" />
            </div>
          </div>
          <div
            ref={progressBarRef}
            className="lux-player-timeline-interaction"
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
          />
          <PlayerChapterTimeline segments={chapters} duration={duration} onSeek={onChapterSeek} />
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

          {episodeNavigation ? (
            <>
              <button
                type="button"
                className="lux-player-action-btn lux-player-episode-nav-btn"
                aria-label="上一集"
                title="上一集"
                disabled={!episodeNavigation.previousEpisodeId}
                onClick={() => {
                  if (episodeNavigation.previousEpisodeId) onNavigateToEpisode(episodeNavigation.previousEpisodeId);
                }}
              >
                <SkipBack size={20} aria-hidden="true" />
              </button>
              <button
                type="button"
                className="lux-player-action-btn lux-player-episode-nav-btn"
                aria-label="下一集"
                title="下一集"
                disabled={!episodeNavigation.nextEpisodeId}
                onClick={() => {
                  if (episodeNavigation.nextEpisodeId) onNavigateToEpisode(episodeNavigation.nextEpisodeId);
                }}
              >
                <SkipForward size={20} aria-hidden="true" />
              </button>
            </>
          ) : null}

          {onSeekBy ? (
            <>
              <button
                type="button"
                className="lux-player-action-btn lux-player-seek-btn"
                aria-label="后退 10 秒"
                title="后退 10 秒"
                onClick={() => onSeekBy(-10)}
              >
                <RotateCcw size={20} aria-hidden="true" />
              </button>
              <button
                type="button"
                className="lux-player-action-btn lux-player-seek-btn"
                aria-label="前进 10 秒"
                title="前进 10 秒"
                onClick={() => onSeekBy(10)}
              >
                <RotateCw size={20} aria-hidden="true" />
              </button>
            </>
          ) : null}

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
                style={{ "--lux-volume-percent": volumePercent } as CSSProperties}
                className="lux-player-volume-slider"
                aria-label="音量调节"
              />
            </div>
          </div>

          {subtitleMenu ? (
            <div className="lux-player-track-anchor">
              <button
                type="button"
                className={`lux-player-action-btn lux-player-subtitle-btn ${subtitleActive ? "is-active" : ""} ${openMenu === "subtitle" ? "is-active" : ""}`}
                aria-label="字幕"
                aria-haspopup="dialog"
                aria-expanded={openMenu === "subtitle"}
                title="字幕"
                onClick={() => toggleMenu("subtitle")}
              >
                <Subtitles size={20} aria-hidden="true" />
              </button>
              {openMenu === "subtitle" ? (
                <div className="lux-player-track-popover" role="dialog" aria-label="字幕">
                  {subtitleMenu}
                </div>
              ) : null}
            </div>
          ) : null}
        </div>

        <div className="lux-player-controls-middle" />

        <div className="lux-player-controls-right">
          {audioMenu ? (
            <div className="lux-player-track-anchor">
              <button
                type="button"
                className={`lux-player-action-btn lux-player-audio-btn ${openMenu === "audio" ? "is-active" : ""}`}
                aria-label="音轨"
                aria-haspopup="dialog"
                aria-expanded={openMenu === "audio"}
                title="音轨"
                onClick={() => toggleMenu("audio")}
              >
                <AudioLines size={20} aria-hidden="true" />
              </button>
              {openMenu === "audio" ? (
                <div className="lux-player-track-popover" role="dialog" aria-label="音轨">
                  {audioMenu}
                </div>
              ) : null}
            </div>
          ) : null}
          <button
            type="button"
            className={`lux-player-action-btn lux-player-speed-btn ${playbackRate !== 1 ? "is-active" : ""}`}
            aria-label="倍速"
            title={`倍速：${rateLabel(playbackRate)}`}
            onClick={cycleRate}
          >
            <Gauge size={20} aria-hidden="true" />
          </button>
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
          {airPlayAvailable ? (
            <button type="button" className="lux-player-action-btn lux-player-mobile-top-control" aria-label="AirPlay" title="AirPlay" onClick={onAirPlay}>
              <Airplay size={20} aria-hidden="true" />
            </button>
          ) : null}
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
          <button type="button" className="lux-player-action-btn" aria-label={fullscreen ? "退出全屏" : "全屏"} title={fullscreen ? "退出全屏 (F)" : "全屏 (F)"} onClick={onToggleFullscreen}>
            {fullscreen ? <Minimize size={20} aria-hidden="true" /> : <Maximize size={20} aria-hidden="true" />}
          </button>
        </div>
      </div>
    </div>
  );
}
