import { PlayerOptionList } from "./player-option-list";
import {
  shouldShowQualitySelector,
  type QualityOption,
} from "../playback-quality";

export type PlayerQualitySection = {
  options: readonly QualityOption[];
  /** Value of the current option; the empty string means 原画（自动）. */
  selectedValue: string;
  switching?: boolean;
  /** Disabled while the other session-rebuilding selector is switching. */
  disabled?: boolean;
  /** Failure/rollback message owned by the player's quality switch hook. */
  notice?: string | null;
  /** Effective status from the session's `bitrate` response block. */
  status: string;
  onSelect: (value: string) => void;
};

/**
 * Independent 画质 (resolution ceiling) slot for the player settings panel. Like
 * the audio track slot it owns no playback state: the player passes the
 * generated resolution tiers, the current choice, the effective status and the
 * switch callback.
 *
 * `.strm` sources are Direct-only and the API ignores `maxHeight` for them, so
 * `qualityOptions` returns only the automatic choice and this renders nothing.
 */
export function PlayerQualitySelector({
  options,
  selectedValue,
  switching = false,
  disabled = false,
  notice = null,
  status,
  onSelect,
}: PlayerQualitySection) {
  if (!shouldShowQualitySelector(options)) return null;
  const current = options.find((option) => option.isCurrent)?.value ?? selectedValue;

  return (
    <div className="lux-player-settings-section" data-settings-section="quality">
      <h4 className="lux-player-settings-label">清晰度</h4>
      <PlayerOptionList
        label="选择画质"
        value={current}
        disabled={switching || disabled}
        options={options.map((option) => ({
          value: option.value,
          label: option.height !== null && option.isCurrent ? `${option.label}（当前）` : option.label,
        }))}
        onSelect={onSelect}
      />
      <p className="lux-player-caption-status" role="status">
        {switching ? "正在切换画质…" : notice ?? status}
      </p>
      {switching || notice ? null : (
        <p className="lux-player-caption-status">
          切换画质会以当前位置重建播放会话；选择会写入地址栏，刷新后保持。
        </p>
      )}
    </div>
  );
}
