type PlayerAutoNextCardProps = {
  secondsLeft: number;
  onPlayNow: () => void;
  onCancel: () => void;
};

/**
 * Countdown card shown when playback ended and the preference offers the next
 * episode. It only renders the countdown state; the page owns the timer and the
 * navigation.
 */
export function PlayerAutoNextCard({
  secondsLeft,
  onPlayNow,
  onCancel,
}: PlayerAutoNextCardProps) {
  const remaining = Math.max(1, Math.round(secondsLeft));

  return (
    <div className="lux-player-auto-next" role="group" aria-label="自动连播下一集">
      <p
        className="lux-player-auto-next-text"
        role="status"
        aria-live="polite"
        aria-atomic="true"
      >
        {`下一集将在 ${remaining} 秒后播放`}
      </p>
      <div className="lux-player-auto-next-actions">
        <button
          type="button"
          className="lux-player-auto-next-play"
          aria-label="立即播放下一集"
          onClick={onPlayNow}
        >
          立即播放
        </button>
        <button
          type="button"
          className="lux-player-auto-next-cancel"
          aria-label="取消自动连播"
          onClick={onCancel}
        >
          取消
        </button>
      </div>
    </div>
  );
}
