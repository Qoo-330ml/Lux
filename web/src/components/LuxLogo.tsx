import "./LuxLogo.css";

type LuxLogoProps = {
  className?: string;
  alt?: string;
};

export function LuxLogo({ className = "", alt = "" }: LuxLogoProps) {
  return (
    <span className={`lux-theme-logo ${className}`.trim()}>
      <img className="lux-theme-logo-image lux-theme-logo-light" src="/logo-black.svg" alt={alt} />
      <img className="lux-theme-logo-image lux-theme-logo-dark" src="/logo-white.svg" alt={alt} />
    </span>
  );
}
