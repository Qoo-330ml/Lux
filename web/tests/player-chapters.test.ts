import { describe, expect, it } from "vitest";
import type { MediaChapter } from "../src/lib/api/types";
import { normalizePlayerChapters } from "../src/features/player/player-chapters";

function chapter(
  startPositionTicks: number,
  markerType: string,
  chapterIndex: number,
  name?: string | null,
): MediaChapter {
  return { startPositionTicks, markerType, chapterIndex, name };
}

describe("LuxPlayer chapter normalization", () => {
  it("sorts, deduplicates, bounds, and pairs explicit intro markers", () => {
    const result = normalizePlayerChapters([
      chapter(80_000_000, "CREDITS_START", 3),
      chapter(60_000_000, "INTRO_END", 2, "片头结束"),
      chapter(20_000_000, "INTRO_START", 1, "片头开始"),
      chapter(20_000_000, "INTRO_START", 9, "重复片头"),
      chapter(0, "CHAPTER", 0, "开场"),
      chapter(-1, "CHAPTER", 4, "非法"),
      chapter(120_000_000, "CHAPTER", 5, "超出时长"),
    ], 10);

    expect(result.segments).toEqual([
      expect.objectContaining({ start: 0, end: 2, title: "开场", markerType: "CHAPTER" }),
      expect.objectContaining({ start: 2, end: 6, title: "片头开始", markerType: "INTRO_START" }),
      expect.objectContaining({ start: 6, end: 8, title: "片头结束", markerType: "INTRO_END" }),
      expect.objectContaining({ start: 8, end: 10, title: "片尾开始", markerType: "CREDITS_START" }),
    ]);
    expect(result.introRanges).toEqual([{ start: 2, end: 6 }]);
  });

  it("does not invent a skip range for missing or inverted intro markers", () => {
    expect(normalizePlayerChapters([
      chapter(5_000_000, "INTRO_END", 0),
      chapter(20_000_000, "INTRO_START", 1),
    ], 10).introRanges).toEqual([]);
    expect(normalizePlayerChapters([
      chapter(20_000_000, "INTRO_START", 0),
      chapter(10_000_000, "INTRO_END", 1),
    ], 10).introRanges).toEqual([]);
  });

  it("keeps the payload bounded even when a legacy response is oversized", () => {
    const chapters = Array.from({ length: 300 }, (_, index) => chapter(index * 100_000, "CHAPTER", index));
    const result = normalizePlayerChapters(chapters, 60);
    expect(result.segments.length).toBeLessThanOrEqual(256);
    expect(result.segments[0]?.start).toBe(0);
  });
});
