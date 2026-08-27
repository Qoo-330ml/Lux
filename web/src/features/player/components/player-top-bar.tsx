import { ArrowLeft, Maximize, Minimize, Settings2 } from "lucide-react";

export type PlayerSourceOption = {
  id: string;
  label: string;
  detail: string;
};

type PlayerTopBarProps = {
  title: string;
  badge?: string | null;
  subtitle?: string | null;
  sources: readonly PlayerSourceOption[];
  selectedSourceId: string;
  settingsOpen: boolean;
  fullscreen: boolean;
  onBack: () => void;
  onSourceChange: (sourceId: string) => void;
  onToggleSettings: () => void;
  onToggleFullscreen: () => void;
};

export function PlayerTopBar({
  title,
  badge,
  subtitle,
  sources,
  selectedSourceId,
  settingsOpen,
  fullscreen,
  onBack,
  onSourceChange,
  onToggleSettings,
  onToggleFullscreen,
}: PlayerTopBarProps) {
  return (
    <div className="lux-player-topbar">
      <div className="lux-player-topbar-left">
        <button
          type="button"
          className="lux-player-icon-btn"
          aria-label="返回"
          title="返回"
          onClick={onBack}
        >
          <ArrowLeft size={20} aria-hidden="true" />
        </button>
        <div className="lux-player-meta">
          <div className="lux-player-title-row">
            <span className="lux-player-title">{title}</span>
            {badge ? <span className="lux-player-badge">{badge}</span> : null}
          </div>
          {subtitle ? <span className="lux-player-subtitle">{subtitle}</span> : null}
        </div>
      </div>

      <div className="lux-player-topbar-right">
        {sources.length > 1 ? (
          <div className="lux-player-source-selector">
            <select
              aria-label="选择播放源"
              value={selectedSourceId}
              onChange={(event) => onSourceChange(event.target.value)}
            >
              {sources.map((source) => (
                <option key={source.id} value={source.id}>
                  {source.label} ({source.detail})
                </option>
              ))}
            </select>
          </div>
        ) : null}

        <button
          type="button"
          className={`lux-player-icon-btn ${settingsOpen ? "is-active" : ""}`}
          aria-label="播放器设置"
          aria-expanded={settingsOpen}
          title="设置"
          onClick={onToggleSettings}
        >
          <Settings2 size={20} aria-hidden="true" />
        </button>

        <button
          type="button"
          className="lux-player-icon-btn"
          aria-label={fullscreen ? "退出全屏" : "全屏"}
          title={fullscreen ? "退出全屏 (F)" : "全屏 (F)"}
          onClick={onToggleFullscreen}
        >
          {fullscreen ? <Minimize size={20} aria-hidden="true" /> : <Maximize size={20} aria-hidden="true" />}
        </button>
      </div>
    </div>
  );
}
