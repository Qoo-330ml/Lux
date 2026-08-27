type PlayerMiniProgressBarProps = {
  controlsVisible: boolean;
  currentTime: number;
  duration: number;
  bufferedEnd: number;
};

export function playerProgressPercent(value: number, duration: number) {
  if (!Number.isFinite(duration) || duration <= 0 || !Number.isFinite(value)) return 0;
  return Math.max(0, Math.min(100, (value / duration) * 100));
}

export function PlayerMiniProgressBar({
  controlsVisible,
  currentTime,
  duration,
  bufferedEnd,
}: PlayerMiniProgressBarProps) {
  if (controlsVisible || !Number.isFinite(duration) || duration <= 0) return null;

  return (
    <div
      className="lux-player-mini-progress"
      data-lux-player-mini-progress="true"
      aria-hidden="true"
    >
      <div
        className="lux-player-mini-progress-buffered"
        style={{ width: `${playerProgressPercent(bufferedEnd, duration)}%` }}
      />
      <div
        className="lux-player-mini-progress-played"
        style={{ width: `${playerProgressPercent(currentTime, duration)}%` }}
      />
    </div>
  );
}
