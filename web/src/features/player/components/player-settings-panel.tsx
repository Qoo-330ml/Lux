import { Check, Info, X } from "lucide-react";
import type {
  PlayerAspectRatio,
  PlayerFlip,
  PlayerVideoPresentation,
} from "./player-presentation";
import {
  PlayerQualitySelector,
  type PlayerQualitySection,
} from "./player-quality-selector";

type PlayerPresentationSettings = PlayerVideoPresentation & {
  onToggleLoop: () => void;
  onChangeAspectRatio: (aspectRatio: PlayerAspectRatio) => void;
  onChangeFlip: (flip: PlayerFlip) => void;
};

type PlayerAutoPlaySettings = {
  enabled: boolean;
  onToggle: () => void;
};

type PlayerSettingsPanelProps = {
  /** Independent quality (resolution ceiling) slot; omitted for Direct-only sources. */
  quality?: PlayerQualitySection;
  presentation?: PlayerPresentationSettings;
  autoPlay?: PlayerAutoPlaySettings;
  /** Opens the playback information panel (subtitles, audio and speed live in the control bar). */
  onOpenInfo?: () => void;
  onClose: () => void;
};

const ASPECT_RATIO_OPTIONS: readonly { value: PlayerAspectRatio; label: string }[] = [
  { value: "default", label: "默认" },
  { value: "16:9", label: "16:9" },
  { value: "4:3", label: "4:3" },
  { value: "fill", label: "填充" },
];

const FLIP_OPTIONS: readonly { value: PlayerFlip; label: string }[] = [
  { value: "normal", label: "正常" },
  { value: "horizontal", label: "水平镜像" },
  { value: "vertical", label: "垂直镜像" },
];

export function PlayerSettingsPanel({
  quality,
  presentation,
  autoPlay,
  onOpenInfo,
  onClose,
}: PlayerSettingsPanelProps) {
  return (
    <div className="lux-player-settings-popover" role="dialog" aria-label="播放设置">
      <div className="lux-player-settings-header">
        <span>播放设置</span>
        <button
          type="button"
          className="lux-player-settings-close"
          aria-label="关闭播放设置"
          title="关闭"
          onClick={onClose}
        >
          <X size={16} aria-hidden="true" />
        </button>
      </div>
      {quality ? <PlayerQualitySelector {...quality} /> : null}
      {autoPlay ? (
        <div className="lux-player-settings-section">
          <span className="lux-player-settings-label" id="lux-player-auto-play-label">
            自动连播下一集
          </span>
          <button
            type="button"
            className={`lux-player-speed-pill ${autoPlay.enabled ? "is-active" : ""}`}
            role="switch"
            aria-label="自动连播下一集"
            aria-checked={autoPlay.enabled}
            onClick={autoPlay.onToggle}
          >
            {autoPlay.enabled ? "已开启" : "已关闭"}
            {autoPlay.enabled ? <Check size={14} aria-hidden="true" /> : null}
          </button>
        </div>
      ) : null}
      {presentation ? (
        <>
          <div className="lux-player-settings-section">
            <span className="lux-player-settings-label" id="lux-player-loop-label">循环播放</span>
            <button
              type="button"
              className={`lux-player-speed-pill ${presentation.loop ? "is-active" : ""}`}
              role="switch"
              aria-labelledby="lux-player-loop-label"
              aria-checked={presentation.loop}
              onClick={presentation.onToggleLoop}
            >
              {presentation.loop ? "已开启" : "已关闭"}
              {presentation.loop ? <Check size={14} aria-hidden="true" /> : null}
            </button>
          </div>
          <div className="lux-player-settings-section">
            <span className="lux-player-settings-label" id="lux-player-aspect-ratio-label">长宽比</span>
            <div className="lux-player-speed-grid" role="group" aria-labelledby="lux-player-aspect-ratio-label">
              {ASPECT_RATIO_OPTIONS.map((option) => (
                <button
                  key={option.value}
                  type="button"
                  className={`lux-player-speed-pill ${presentation.aspectRatio === option.value ? "is-active" : ""}`}
                  aria-pressed={presentation.aspectRatio === option.value}
                  onClick={() => presentation.onChangeAspectRatio(option.value)}
                >
                  {option.label}
                </button>
              ))}
            </div>
          </div>
          <div className="lux-player-settings-section">
            <span className="lux-player-settings-label" id="lux-player-flip-label">画面翻转</span>
            <div className="lux-player-speed-grid" role="group" aria-labelledby="lux-player-flip-label">
              {FLIP_OPTIONS.map((option) => (
                <button
                  key={option.value}
                  type="button"
                  className={`lux-player-speed-pill ${presentation.flip === option.value ? "is-active" : ""}`}
                  aria-pressed={presentation.flip === option.value}
                  onClick={() => presentation.onChangeFlip(option.value)}
                >
                  {option.label}
                </button>
              ))}
            </div>
          </div>
        </>
      ) : null}
      {onOpenInfo ? (
        <div className="lux-player-settings-section">
          <button type="button" className="lux-player-settings-info-row" onClick={onOpenInfo}>
            <Info size={16} aria-hidden="true" />
            播放信息
          </button>
        </div>
      ) : null}
    </div>
  );
}
