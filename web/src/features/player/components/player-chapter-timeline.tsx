import type { PlayerChapterSegment, PlayerIntroRange } from "../player-chapters";
import { creditsSkipAction, shouldShowSkipCredits } from "../player-autoplay";

type PlayerChapterTimelineProps = {
  segments: readonly PlayerChapterSegment[];
  duration: number;
  onSeek: (seconds: number) => void;
};

export function PlayerChapterTimeline({ segments, duration, onSeek }: PlayerChapterTimelineProps) {
  if (!segments.length || !Number.isFinite(duration) || duration <= 0) return null;

  return (
    <div className="lux-player-chapter-rail" role="group" aria-label="章节">
      {segments.map((segment) => (
        <button
          key={segment.id}
          type="button"
          className="lux-player-chapter-segment"
          style={{
            left: `${(segment.start / duration) * 100}%`,
            width: "4px",
          }}
          data-marker-type={segment.markerType}
          data-chapter-start={segment.start}
          aria-label={`章节：${segment.title}`}
          title={`${segment.title} · ${formatChapterTime(segment.start)}`}
          onClick={() => onSeek(segment.start)}
        />
      ))}
    </div>
  );
}

type PlayerIntroSkipProps = {
  currentTime: number;
  introSkip: PlayerIntroRange | null;
  onSkip: (seconds: number) => void;
};

export function PlayerIntroSkip({ currentTime, introSkip, onSkip }: PlayerIntroSkipProps) {
  if (
    !introSkip
    || !Number.isFinite(currentTime)
    || currentTime < introSkip.start
    || currentTime >= introSkip.end
  ) return null;

  return (
    <button
      type="button"
      className="lux-player-skip-intro"
      aria-label="跳过片头"
      onClick={() => onSkip(introSkip.end)}
    >
      跳过片头
    </button>
  );
}

type PlayerSkipCreditsProps = {
  currentTime: number;
  duration: number;
  creditsStart: number | null;
  hasNextEpisode: boolean;
  onSkip: () => void;
};

/**
 * Skip credits shares the skip intro slot: both markers never overlap in time,
 * so one position is enough. The action itself (next episode or finish the
 * media) is decided by the page through the pure helper.
 */
export function PlayerSkipCredits({
  currentTime,
  duration,
  creditsStart,
  hasNextEpisode,
  onSkip,
}: PlayerSkipCreditsProps) {
  if (!shouldShowSkipCredits({ creditsStart, currentTime, duration })) return null;
  if (!creditsSkipAction({ creditsStart, duration, hasNextEpisode })) return null;

  return (
    <button
      type="button"
      className="lux-player-skip-credits"
      aria-label="跳过片尾"
      title={hasNextEpisode ? "跳过片尾并播放下一集" : "跳到结尾"}
      onClick={onSkip}
    >
      跳过片尾
    </button>
  );
}

function formatChapterTime(seconds: number) {
  const total = Math.max(0, Math.floor(seconds));
  const minutes = Math.floor(total / 60);
  const remainder = total % 60;
  return `${String(minutes).padStart(2, "0")}:${String(remainder).padStart(2, "0")}`;
}
