import { useEffect, useState } from "react";

type PlayerAutoNextCardProps = {
  secondsLeft: number;
  onPlayNow: () => void;
  onCancel: () => void;
};

/** The single announcement this card makes, independent of the countdown. */
const AUTO_NEXT_ANNOUNCEMENT = "下一集即将自动播放，可以立即播放或取消。";

/**
 * Countdown card shown when playback ended and the preference offers the next
 * episode. It only renders the countdown state; the page owns the timer and the
 * navigation.
 *
 * Accessibility: the live region exists once per card lifetime and never carries
 * the seconds. The visible countdown is `aria-hidden`, so the card is announced
 * when it appears instead of interrupting the viewer with a new number every
 * second, and leaving it (播放 or 取消) is a user action that needs no
 * announcement of its own. The announcement text is written after mount so the
 * insertion is a real content change for assistive technology.
 */
export function PlayerAutoNextCard({
  secondsLeft,
  onPlayNow,
  onCancel,
}: PlayerAutoNextCardProps) {
  const remaining = Math.max(1, Math.round(secondsLeft));
  const [announcement, setAnnouncement] = useState("");

  useEffect(() => {
    setAnnouncement(AUTO_NEXT_ANNOUNCEMENT);
  }, []);

  return (
    <div className="lux-player-auto-next" role="group" aria-label="自动连播下一集">
      <p className="lux-player-auto-next-text">
        <span className="lux-visually-hidden" aria-live="polite">
          {announcement}
        </span>
        <span aria-hidden="true">{`下一集将在 ${remaining} 秒后播放`}</span>
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
