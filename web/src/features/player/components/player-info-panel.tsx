import { Check, Copy, X } from "lucide-react";
import { useState } from "react";
import type { PlayerInfoGroup } from "../player-stats";

type PlayerInfoPanelProps = {
  groups: PlayerInfoGroup[];
  copyText: string;
  onClose: () => void;
};

/**
 * The player's "stats for nerds" overlay.
 *
 * It is a read-only view of the playback session: opening it must not change playback, and every
 * value inside it is selectable so users can paste a session dump into a bug report.
 */
export function PlayerInfoPanel({ groups, copyText, onClose }: PlayerInfoPanelProps) {
  const [copied, setCopied] = useState(false);

  return (
    <section
      className="lux-player-info"
      role="dialog"
      aria-label="播放信息"
      aria-modal="false"
      onClick={(event) => event.stopPropagation()}
      onDoubleClick={(event) => event.stopPropagation()}
      onContextMenu={(event) => event.preventDefault()}
    >
      <header className="lux-player-info-head">
        <h2>播放信息</h2>
        <div className="lux-player-info-actions">
          <button
            type="button"
            className="lux-player-info-action"
            aria-label="复制播放信息"
            title="复制播放信息"
            onClick={() => {
              void (async () => {
                try {
                  await navigator.clipboard.writeText(copyText);
                  setCopied(true);
                } catch {
                  setCopied(false);
                }
              })();
            }}
          >
            {copied ? <Check size={15} aria-hidden="true" /> : <Copy size={15} aria-hidden="true" />}
          </button>
          <button
            type="button"
            className="lux-player-info-action"
            aria-label="关闭播放信息"
            title="关闭 (Esc)"
            onClick={onClose}
          >
            <X size={16} aria-hidden="true" />
          </button>
        </div>
      </header>
      <div className="lux-player-info-body">
        {groups.map((group) => (
          <section key={group.title} className="lux-player-info-group" aria-label={group.title}>
            <h3>{group.title}</h3>
            <dl>
              {group.rows.map((row) => (
                <div key={`${group.title}-${row.label}`} className="lux-player-info-row">
                  <dt>{row.label}</dt>
                  <dd>{row.value}</dd>
                </div>
              ))}
            </dl>
          </section>
        ))}
      </div>
    </section>
  );
}
