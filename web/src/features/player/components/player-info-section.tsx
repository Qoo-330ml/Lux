import { Check, Copy } from "lucide-react";
import { useState } from "react";
import type { PlayerInfoGroup } from "../player-stats";

type PlayerInfoSectionProps = {
  groups: PlayerInfoGroup[];
  copyText: string;
};

/**
 * The player's "stats for nerds" view, shown in the settings drawer's 播放信息 tab.
 *
 * It is a read-only view of the playback session: opening it must not change playback, and every
 * value is selectable so users can paste a session dump into a bug report.
 */
export function PlayerInfoSection({ groups, copyText }: PlayerInfoSectionProps) {
  const [copied, setCopied] = useState(false);

  return (
    <div className="lux-player-info-section" data-settings-section="info">
      <div className="lux-player-info-section-head">
        <h4 className="lux-player-settings-label">播放信息</h4>
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
      </div>
      {groups.map((group) => (
        <section key={group.title} className="lux-player-info-group" aria-label={group.title}>
          <h5>{group.title}</h5>
          <dl className="lux-player-info-list">
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
  );
}
