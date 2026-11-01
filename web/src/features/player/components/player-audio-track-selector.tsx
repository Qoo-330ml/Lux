import { LuxSelectField } from "../../../components/ui";
import {
  shouldShowAudioTrackSelector,
  type AudioTrackOption,
} from "../audio-track-selection";

export type PlayerAudioTrackSection = {
  tracks: readonly AudioTrackOption[];
  selectedIndex: string | null;
  switching?: boolean;
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
      <label className="lux-player-settings-label" htmlFor="lux-player-audio-select">音轨</label>
      <LuxSelectField
        id="lux-player-audio-select"
        className="lux-player-caption-select"
        aria-label="选择音轨"
        title="选择音轨"
        value={current}
        disabled={switching}
        onChange={(event) => onSelect(event.target.value)}
      >
        {tracks.map((track) => (
          <option key={track.index} value={track.index}>
            {track.isCurrent ? `${track.label}（当前）` : track.label}
          </option>
        ))}
      </LuxSelectField>
      {status ? (
        <p className="lux-player-caption-status" role="status">{status}</p>
      ) : (
        <p className="lux-player-caption-status">切换音轨会以当前位置重建播放会话。</p>
      )}
    </div>
  );
}
