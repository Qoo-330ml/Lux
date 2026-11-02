import { LuxSelectField } from "../../../components/ui";
import type { PlayerCaptionOption } from "./player-captions";
import {
  CAPTION_OFFSET_MAX,
  CAPTION_OFFSET_MIN,
  CAPTION_OFFSET_STEP,
  formatCaptionOffset,
  normalizeCaptionOffset,
} from "../caption-offset";

export type PlayerSubtitleMenuProps = {
  captions?: readonly PlayerCaptionOption[];
  selectedCaptionId?: string | null;
  captionStatus?: string | null;
  onSelectCaption?: (id: string | null) => void;
  captionOffset?: number;
  onChangeCaptionOffset?: (offset: number) => void;
};

/**
 * Subtitle choice and offset, shown in the control bar's subtitle popover.
 * It owns no playback state; the player passes captions and callbacks in.
 */
export function PlayerSubtitleMenu({
  captions = [],
  selectedCaptionId = null,
  captionStatus = null,
  onSelectCaption = () => undefined,
  captionOffset = 0,
  onChangeCaptionOffset = () => undefined,
}: PlayerSubtitleMenuProps) {
  const captionHelp = captionStatus
    ?? (captions.length === 0
      ? "当前版本没有字幕轨。"
      : captions.every((caption) => !caption.available)
        ? "当前版本没有可用的 WebVTT 字幕。"
        : null);

  return (
    <div className="lux-player-settings-section" data-settings-section="subtitle">
      <label className="lux-player-settings-label" htmlFor="lux-player-caption-select">字幕</label>
      <LuxSelectField
        id="lux-player-caption-select"
        className="lux-player-caption-select"
        aria-label="选择字幕"
        value={selectedCaptionId ?? ""}
        onChange={(event) => {
          const value = event.target.value;
          onSelectCaption(value === "" ? null : value);
        }}
      >
        <option value="">关闭字幕</option>
        {captions.map((caption) => (
          <option key={caption.id} value={caption.id} disabled={!caption.available}>
            {caption.unavailableReason
              ? `${caption.label}（${caption.unavailableReason}）`
              : caption.label}
          </option>
        ))}
      </LuxSelectField>
      {captionHelp ? <p className="lux-player-caption-status" role="status">{captionHelp}</p> : null}
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
  );
}
