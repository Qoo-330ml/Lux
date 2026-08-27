import type { MediaChapter } from "../../lib/api/types";

export const PLAYER_CHAPTER_LIMIT = 256;
const TICKS_PER_SECOND = 10_000_000;

export type PlayerChapterSegment = {
  id: string;
  start: number;
  end: number;
  title: string;
  markerType: string;
  chapterIndex: number;
};

export type PlayerIntroRange = {
  start: number;
  end: number;
};

export type PlayerChapterTimeline = {
  segments: PlayerChapterSegment[];
  introRanges: PlayerIntroRange[];
};

export function normalizePlayerChapters(
  chapters: readonly MediaChapter[] | undefined,
  duration: number,
): PlayerChapterTimeline {
  if (!Number.isFinite(duration) || duration <= 0 || !chapters?.length) {
    return { segments: [], introRanges: [] };
  }

  const sorted = chapters
    .filter((chapter) => (
      Number.isFinite(chapter.startPositionTicks)
      && chapter.startPositionTicks >= 0
      && Number.isFinite(chapter.chapterIndex)
      && chapter.chapterIndex >= 0
      && typeof chapter.markerType === "string"
    ))
    .map((chapter) => ({
      ...chapter,
      start: chapter.startPositionTicks / TICKS_PER_SECOND,
      title: chapterTitle(chapter),
    }))
    .filter((chapter) => chapter.start < duration)
    .sort((left, right) => (
      left.start - right.start
      || markerRank(left.markerType) - markerRank(right.markerType)
      || left.chapterIndex - right.chapterIndex
      || left.title.localeCompare(right.title, "zh-CN")
    ));

  const unique = sorted.filter((chapter, index, values) => {
    if (index === 0) return true;
    const previous = values[index - 1];
    return previous.startPositionTicks !== chapter.startPositionTicks
      || previous.markerType !== chapter.markerType;
  }).slice(0, PLAYER_CHAPTER_LIMIT);

  const segments = unique.flatMap((chapter, index) => {
    const end = unique[index + 1]?.start ?? duration;
    if (end <= chapter.start) return [];
    return [{
      id: `chapter-${chapter.startPositionTicks}-${chapter.markerType}-${chapter.chapterIndex}`,
      start: chapter.start,
      end,
      title: chapter.title,
      markerType: chapter.markerType,
      chapterIndex: chapter.chapterIndex,
    }];
  });

  return {
    segments,
    introRanges: explicitIntroRanges(unique, duration),
  };
}

function explicitIntroRanges(
  chapters: readonly { start: number; markerType: string }[],
  duration: number,
) {
  const starts: number[] = [];
  const ranges: PlayerIntroRange[] = [];
  for (const chapter of chapters) {
    if (chapter.markerType === "INTRO_START") {
      starts.push(chapter.start);
      continue;
    }
    if (chapter.markerType !== "INTRO_END") continue;
    let matchingStart = -1;
    for (let index = starts.length - 1; index >= 0; index -= 1) {
      if (starts[index] < chapter.start) {
        matchingStart = starts[index];
        starts.splice(index, 1);
        break;
      }
    }
    if (matchingStart >= 0 && chapter.start <= duration) {
      ranges.push({ start: matchingStart, end: chapter.start });
    }
  }
  return ranges;
}

function chapterTitle(chapter: MediaChapter) {
  const name = chapter.name?.trim();
  if (name) return name;
  switch (chapter.markerType) {
    case "INTRO_START": return "片头开始";
    case "INTRO_END": return "片头结束";
    case "CREDITS_START": return "片尾开始";
    default: return `第 ${chapter.chapterIndex + 1} 章`;
  }
}

function markerRank(markerType: string) {
  switch (markerType) {
    case "INTRO_START": return 0;
    case "INTRO_END": return 1;
    case "CREDITS_START": return 2;
    default: return 99;
  }
}
