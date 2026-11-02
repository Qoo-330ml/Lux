import { describe, expect, it } from "vitest";
import {
  DANMAKU_LANE_GAP,
  FIXED_DURATION_SECONDS,
  SCROLL_DURATION_SECONDS,
  createDanmakuScheduler,
  createTextMeasurer,
  laneGeometry,
  type MeasureText,
} from "../src/features/player/danmaku";
import type { LuxDanmakuEntry } from "../src/features/player/danmaku";

function entry(index: number, start: number, text = "弹幕"): LuxDanmakuEntry {
  return { id: `danmaku-${index}`, start, mode: "scroll", text, color: "#ffffff", fontSize: 25 };
}

/** A deterministic stand-in for canvas measurement: twenty pixels per character. */
const measure: MeasureText = (text) => text.length * 20;

const viewport = { width: 800, height: 450 };

describe("LuxPlayer danmaku text measurement", () => {
  it("returns a positive width without a canvas and caches repeated measurements", () => {
    const measurer = createTextMeasurer();
    const first = measurer("弹幕宽度", 25);
    const second = measurer("弹幕宽度", 25);

    expect(first).toBeGreaterThan(0);
    expect(second).toBe(first);
    expect(measurer("更长的一段弹幕文本", 25)).toBeGreaterThan(measurer("短", 25));
    expect(measurer("弹幕宽度", 50)).toBeGreaterThan(first);
  });
});

describe("LuxPlayer danmaku scheduler", () => {
  it("keeps scrolling, top, and bottom danmaku on independent lanes", () => {
    const entries: LuxDanmakuEntry[] = [
      entry(0, 0),
      entry(1, 0.1),
      { ...entry(2, 1, "顶部"), mode: "top" },
      { ...entry(3, 2, "底部"), mode: "bottom" },
    ];
    const scheduler = createDanmakuScheduler(entries, { viewport, measure });

    const placements = scheduler.place(2.5);
    const scroll = placements.filter((placement) => placement.mode === "scroll");
    const top = placements.find((placement) => placement.mode === "top");
    const bottom = placements.find((placement) => placement.mode === "bottom");

    // Two scrolling entries started 100ms apart cannot share a lane without overlapping.
    expect(new Set(scroll.map((placement) => placement.lane)).size).toBe(scroll.length);
    expect(top?.lane).toBe(0);
    expect(bottom?.lane).toBe(0);
    // Fixed danmaku never collide with the scrolling band or each other.
    const ys = new Set(placements.map((placement) => placement.y));
    expect(ys.size).toBe(placements.length);
  });

  it("uses the measured width for scroll geometry instead of a character-count guess", () => {
    const scheduler = createDanmakuScheduler([entry(0, 0, "12345")], { viewport, measure });

    const atStart = scheduler.place(0)[0]!;
    expect(atStart.width).toBe(100 + 8);
    // Scroll entry starts at the right edge and moves by (viewport + width) over its lifetime.
    expect(atStart.x).toBeCloseTo(viewport.width, 5);
    const halfway = scheduler.place(SCROLL_DURATION_SECONDS / 2)[0]!;
    expect(halfway.x).toBeCloseTo(viewport.width - 0.5 * (viewport.width + atStart.width), 5);
  });

  it("never lets scrolling danmaku overlap or catch up inside one lane", () => {
    // Long strings measured at 600px: a short delay would make two of them collide.
    const entries = Array.from({ length: 40 }, (_, index) => entry(index, index * 0.4, "长".repeat(30)));
    const scheduler = createDanmakuScheduler(entries, { viewport, measure, maxVisible: 80 });

    for (let time = 0; time < 20; time += 0.2) {
      const placements = scheduler
        .place(time)
        .filter((placement) => placement.mode === "scroll")
        .sort((left, right) => left.lane - right.lane || left.start - right.start);
      for (let index = 1; index < placements.length; index += 1) {
        const previous = placements[index - 1]!;
        const current = placements[index]!;
        if (previous.lane !== current.lane) continue;
        const previousRight = previous.x + previous.width;
        expect(current.x).toBeGreaterThanOrEqual(previousRight - 0.5);
      }
    }
  });

  it("allocates lanes on demand from the current playback window", () => {
    const scheduler = createDanmakuScheduler([entry(0, 0), entry(1, 5), entry(2, 10)], {
      viewport,
      measure,
    });

    // At the current playback time only the entries whose 8s window still covers it are placed;
    // the scheduler never precomputes lanes for the whole file.
    expect(scheduler.place(10).map((placement) => placement.id)).toEqual(["danmaku-1", "danmaku-2"]);
    expect(scheduler.place(10.5).map((placement) => placement.id)).toEqual(["danmaku-1", "danmaku-2"]);
    expect(scheduler.place(13).map((placement) => placement.id)).toEqual(["danmaku-2"]);
    expect(scheduler.place(18)).toEqual([]);
  });

  it("recomputes lanes after a seek so a rewound window matches a fresh schedule", () => {
    const entries = Array.from({ length: 60 }, (_, index) => entry(index, index * 0.5));
    const scheduler = createDanmakuScheduler(entries, { viewport, measure });
    const fresh = createDanmakuScheduler(entries, { viewport, measure });

    scheduler.place(25);
    scheduler.place(25.1);

    // Seeking backwards must drop the stale lane state instead of reusing lanes assigned at 25s.
    expect(scheduler.place(6)).toEqual(fresh.place(6));
    // Seeking forwards past the whole file must land on an empty window, not on stale entries.
    expect(scheduler.place(600)).toEqual([]);

    const afterForwardSeek = createDanmakuScheduler(entries, { viewport, measure });
    expect(scheduler.place(12)).toEqual(afterForwardSeek.place(12));
  });

  it("bounds the visible window and drops entries that have no free lane", () => {
    const { scrollLaneCount } = laneGeometry(viewport.height);
    const dense = Array.from({ length: 400 }, (_, index) => entry(index, 0, "宽".repeat(20)));
    const scheduler = createDanmakuScheduler(dense, { viewport, measure });

    const placements = scheduler.place(0);
    // Only one danmaku fits per lane at the same instant; the excess is dropped, never stacked.
    expect(placements).toHaveLength(scrollLaneCount);
    expect(new Set(placements.map((placement) => placement.lane)).size).toBe(scrollLaneCount);
  });

  it("caps the number of simultaneously rendered danmaku", () => {
    const dense = Array.from({ length: 400 }, (_, index) => entry(index, index * 0.01));
    const scheduler = createDanmakuScheduler(dense, { viewport, measure, maxVisible: 3 });

    expect(scheduler.place(1)).toHaveLength(3);
  });

  it("keeps danmaku inside the viewport safe bands", () => {
    const geometry = laneGeometry(590);
    expect(geometry.fixedLaneCount).toBeGreaterThanOrEqual(2);
    expect(geometry.scrollLaneCount).toBeGreaterThanOrEqual(4);

    const scheduler = createDanmakuScheduler(
      [
        { ...entry(0, 0), mode: "top" },
        { ...entry(1, 0), mode: "bottom" },
        entry(2, 0),
      ],
      { viewport: { width: 390, height: 590 }, measure },
    );
    const placements = scheduler.place(0.5);
    const top = placements.find((placement) => placement.mode === "top")!;
    const bottom = placements.find((placement) => placement.mode === "bottom")!;
    const scroll = placements.find((placement) => placement.mode === "scroll")!;

    expect(top.y).toBe(8);
    expect(bottom.y).toBeLessThan(590);
    expect(scroll.y).toBeGreaterThanOrEqual(geometry.fixedBandHeight);
    expect(scroll.y).toBeLessThan(bottom.y);
    expect(SCROLL_DURATION_SECONDS).toBe(8);
    expect(FIXED_DURATION_SECONDS).toBe(4);
    expect(DANMAKU_LANE_GAP).toBeGreaterThan(0);
  });

  it("reconfigures lane counts when the viewport changes", () => {
    const entries = Array.from({ length: 8 }, (_, index) => entry(index, 0));
    const scheduler = createDanmakuScheduler(entries, { viewport: { width: 1280, height: 720 }, measure });
    const wide = scheduler.place(0);

    scheduler.configure({ width: 320, height: 180 });
    const narrow = scheduler.place(0);

    expect(wide.length).toBeGreaterThan(narrow.length);
    expect(new Set(narrow.map((placement) => placement.lane)).size).toBe(narrow.length);
  });
});
