import { Airplay, ArrowLeft, PictureInPicture2 } from "lucide-react";
import { useState } from "react";

type PlayerTopBarProps = {
  title: string;
  /** Title logo; when absent or failing to load the text title is shown instead. */
  logoUrl?: string | null;
  /** Shown to the right of the title: the year for a film, season and episode for a series. */
  subtitle?: string | null;
  onBack: () => void;
  airPlayAvailable?: boolean;
  onAirPlay?: () => void;
  pictureInPictureEnabled?: boolean;
  onTogglePictureInPicture?: () => void;
};

export function PlayerTopBar({
  title,
  logoUrl = null,
  subtitle,
  onBack,
  airPlayAvailable = false,
  onAirPlay = () => undefined,
  pictureInPictureEnabled = false,
  onTogglePictureInPicture = () => undefined,
}: PlayerTopBarProps) {
  // Remember which URL failed so a different item's logo is tried again.
  const [failedLogoUrl, setFailedLogoUrl] = useState<string | null>(null);
  const showLogo = Boolean(logoUrl) && failedLogoUrl !== logoUrl;
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
            {showLogo && logoUrl ? (
              <img
                className="lux-player-title-logo"
                src={logoUrl}
                alt={title}
                onError={() => setFailedLogoUrl(logoUrl)}
              />
            ) : (
              <span className="lux-player-title">{title}</span>
            )}
            {subtitle ? <span className="lux-player-year">{subtitle}</span> : null}
          </div>
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
