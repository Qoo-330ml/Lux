import type { PlayerChapterSegment, PlayerIntroRange } from "../player-chapters";

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
            width: `${((segment.end - segment.start) / duration) * 100}%`,
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

function formatChapterTime(seconds: number) {
  const total = Math.max(0, Math.floor(seconds));
  const minutes = Math.floor(total / 60);
  const remainder = total % 60;
  return `${String(minutes).padStart(2, "0")}:${String(remainder).padStart(2, "0")}`;
}
