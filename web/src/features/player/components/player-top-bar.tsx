import { ArrowLeft } from "lucide-react";

type PlayerTopBarProps = {
  title: string;
  badge?: string | null;
  subtitle?: string | null;
  onBack: () => void;
};

export function PlayerTopBar({
  title,
  badge,
  subtitle,
  onBack,
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

    </div>
  );
}
