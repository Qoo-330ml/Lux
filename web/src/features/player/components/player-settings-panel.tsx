import { Check, X } from "lucide-react";
import type { PlayerCaptionOption } from "./player-captions";

type PlayerSettingsPanelProps = {
  playbackRates: readonly number[];
  playbackRate: number;
  onChangeRate: (rate: number) => void;
  captions?: readonly PlayerCaptionOption[];
  selectedCaptionStreamIndex?: number | null;
  captionStatus?: string | null;
  onSelectCaption?: (streamIndex: number | null) => void;
  onClose: () => void;
};

export function PlayerSettingsPanel({
  playbackRates,
  playbackRate,
  onChangeRate,
  captions = [],
  selectedCaptionStreamIndex = null,
  captionStatus = null,
  onSelectCaption = () => undefined,
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
