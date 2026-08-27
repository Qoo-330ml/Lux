import { Check, X } from "lucide-react";
import type { PlayerCaptionOption } from "./player-captions";
import {
  CAPTION_OFFSET_MAX,
  CAPTION_OFFSET_MIN,
  CAPTION_OFFSET_STEP,
  formatCaptionOffset,
  normalizeCaptionOffset,
} from "../caption-offset";
import type {
  PlayerAspectRatio,
  PlayerFlip,
  PlayerVideoPresentation,
} from "./player-presentation";

type PlayerPresentationSettings = PlayerVideoPresentation & {
  onToggleLoop: () => void;
  onChangeAspectRatio: (aspectRatio: PlayerAspectRatio) => void;
  onChangeFlip: (flip: PlayerFlip) => void;
};

type PlayerSettingsPanelProps = {
  playbackRates: readonly number[];
  playbackRate: number;
  onChangeRate: (rate: number) => void;
  captions?: readonly PlayerCaptionOption[];
  selectedCaptionStreamIndex?: number | null;
  captionStatus?: string | null;
  onSelectCaption?: (streamIndex: number | null) => void;
  captionOffset?: number;
  onChangeCaptionOffset?: (offset: number) => void;
  presentation?: PlayerPresentationSettings;
  onClose: () => void;
};

const ASPECT_RATIO_OPTIONS: readonly { value: PlayerAspectRatio; label: string }[] = [
  { value: "default", label: "默认" },
  { value: "4:3", label: "4:3" },
  { value: "16:9", label: "16:9" },
];

const FLIP_OPTIONS: readonly { value: PlayerFlip; label: string }[] = [
  { value: "normal", label: "正常" },
  { value: "horizontal", label: "水平镜像" },
  { value: "vertical", label: "垂直镜像" },
];

export function PlayerSettingsPanel({
  playbackRates,
  playbackRate,
  onChangeRate,
  captions = [],
  selectedCaptionStreamIndex = null,
  captionStatus = null,
  onSelectCaption = () => undefined,
  captionOffset = 0,
  onChangeCaptionOffset = () => undefined,
  presentation,
  onClose,
}: PlayerSettingsPanelProps) {
  const captionHelp = captionStatus
    ?? (captions.length === 0
      ? "当前版本没有字幕轨。"
      : captions.every((caption) => !caption.available)
        ? "当前版本没有可用的 WebVTT 字幕。"
        : null);

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
      <div className="lux-player-settings-section">
        <span className="lux-player-settings-label" id="lux-player-speed-label">播放速度</span>
        <div className="lux-player-speed-grid" aria-labelledby="lux-player-speed-label">
          {playbackRates.map((speed) => (
            <button
              key={speed}
              type="button"
              className={`lux-player-speed-pill ${playbackRate === speed ? "is-active" : ""}`}
              aria-pressed={playbackRate === speed}
              onClick={() => onChangeRate(speed)}
            >
              {speed === 1 ? "标准" : `${speed}x`}
              {playbackRate === speed ? <Check size={14} aria-hidden="true" /> : null}
            </button>
          ))}
        </div>
      </div>
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
            <span className="lux-player-settings-label" id="lux-player-aspect-ratio-label">画面比例</span>
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
      <div className="lux-player-settings-section">
        <label className="lux-player-settings-label" htmlFor="lux-player-caption-select">字幕</label>
        <select
          id="lux-player-caption-select"
          className="lux-player-caption-select"
          aria-label="选择字幕"
          value={selectedCaptionStreamIndex ?? ""}
          onChange={(event) => {
            const value = event.target.value;
            onSelectCaption(value === "" ? null : Number(value));
          }}
        >
          <option value="">关闭字幕</option>
          {captions.map((caption) => (
            <option
              key={caption.streamIndex}
              value={caption.streamIndex}
              disabled={!caption.available}
            >
              {caption.unavailableReason
                ? `${caption.label}（${caption.unavailableReason}）`
                : caption.label}
            </option>
          ))}
        </select>
        {captionHelp ? <p className="lux-player-caption-status" role="status">{captionHelp}</p> : null}
      </div>
      <div className="lux-player-settings-section">
        <div className="lux-player-caption-offset-heading">
          <label className="lux-player-settings-label" htmlFor="lux-player-caption-offset">字幕偏移</label>
          <output htmlFor="lux-player-caption-offset" className="lux-player-caption-offset-value">
            {formatCaptionOffset(captionOffset)}
          </output>
        </div>
        <input
          id="lux-player-caption-offset"
          className="lux-player-caption-offset"
          type="range"
          min={CAPTION_OFFSET_MIN}
          max={CAPTION_OFFSET_MAX}
          step={CAPTION_OFFSET_STEP}
          value={normalizeCaptionOffset(captionOffset)}
          aria-label="字幕偏移"
          aria-valuetext={formatCaptionOffset(captionOffset)}
          onChange={(event) => onChangeCaptionOffset(Number(event.target.value))}
        />
        <p className="lux-player-caption-status">正值延后，负值提前；仅影响当前选中的字幕。</p>
      </div>
      <div className="lux-player-settings-section">
        <span className="lux-player-settings-label" id="lux-player-shortcuts-label">快捷键提示</span>
        <div className="lux-player-shortcuts-list" aria-labelledby="lux-player-shortcuts-label">
          <div><span>空格 / K</span><span>播放 / 暂停</span></div>
          <div><span>← / →</span><span>快退 / 快进 10 秒</span></div>
          <div><span>↑ / ↓</span><span>音量调节</span></div>
          <div><span>F</span><span>全屏切换</span></div>
          <div><span>M</span><span>静音切换</span></div>
        </div>
      </div>
    </div>
  );
}
