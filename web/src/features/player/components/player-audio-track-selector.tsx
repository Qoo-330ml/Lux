import { PlayerOptionList } from "./player-option-list";
import {
  shouldShowAudioTrackSelector,
  type AudioTrackOption,
} from "../audio-track-selection";

export type PlayerAudioTrackSection = {
  tracks: readonly AudioTrackOption[];
  selectedIndex: string | null;
  switching?: boolean;
  /** Disabled while the other session-rebuilding selector is switching. */
  disabled?: boolean;
  /** Failure/rollback message owned by the player's switch hook. */
  notice?: string | null;
  onSelect: (index: string) => void;
};

/**
 * Independent audio-track slot for the player settings panel. It owns no
 * playback state: the player passes the resolved stream list, the current
 * selection and the switch callback so a later quality selector can share the
 * same panel without coupling to this component.
 */
export function PlayerAudioTrackSelector({
  tracks,
  selectedIndex,
  switching = false,
  disabled = false,
  notice = null,
  onSelect,
}: PlayerAudioTrackSection) {
  if (!shouldShowAudioTrackSelector(tracks)) return null;
  const current = tracks.find((track) => track.index === selectedIndex)?.index
    ?? tracks.find((track) => track.isCurrent)?.index
    ?? tracks[0]?.index
    ?? "";
  const status = switching ? "正在切换音轨…" : notice;

  return (
    <div className="lux-player-settings-section" data-settings-section="audio-track">
      <h4 className="lux-player-settings-label">音轨</h4>
      <PlayerOptionList
        label="选择音轨"
        value={current}
        disabled={switching || disabled}
        options={tracks.map((track) => ({
          value: track.index,
          label: track.isCurrent ? `${track.label}（当前）` : track.label,
        }))}
        onSelect={onSelect}
      />
      {status ? (
        <p className="lux-player-caption-status" role="status">{status}</p>
      ) : (
        <p className="lux-player-caption-status">切换音轨会以当前位置重建播放会话。</p>
      )}
    </div>
  );
}
