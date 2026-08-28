import { Airplay, ArrowLeft, PictureInPicture2 } from "lucide-react";

type PlayerTopBarProps = {
  title: string;
  subtitle?: string | null;
  onBack: () => void;
  airPlayAvailable?: boolean;
  onAirPlay?: () => void;
  pictureInPictureEnabled?: boolean;
  onTogglePictureInPicture?: () => void;
};

export function PlayerTopBar({
  title,
  subtitle,
  onBack,
  airPlayAvailable = false,
  onAirPlay = () => undefined,
  pictureInPictureEnabled = false,
  onTogglePictureInPicture = () => undefined,
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
          </div>
          {subtitle ? <span className="lux-player-subtitle">{subtitle}</span> : null}
        </div>
      </div>
      {airPlayAvailable || pictureInPictureEnabled ? (
        <div className="lux-player-topbar-actions">
          {airPlayAvailable ? (
            <button type="button" className="lux-player-icon-btn" aria-label="AirPlay" title="AirPlay" onClick={onAirPlay}>
              <Airplay size={20} aria-hidden="true" />
            </button>
          ) : null}
          {pictureInPictureEnabled ? (
            <button type="button" className="lux-player-icon-btn" aria-label="画中画" title="画中画" onClick={onTogglePictureInPicture}>
              <PictureInPicture2 size={19} aria-hidden="true" />
            </button>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
