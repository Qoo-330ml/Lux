import { describe, expect, it } from "vitest";
import {
  DANMAKU_LANE_GAP,
  FIXED_DURATION_SECONDS,
  SCROLL_DURATION_SECONDS,
  createDanmakuScheduler,
  createTextMeasurer,
  laneGeometry,
  matchesDanmakuKeyword,
  type MeasureText,
} from "../src/features/player/danmaku";
import {
  DEFAULT_DANMAKU_LAYOUT,
  type DanmakuLayoutOptions,
} from "../src/features/player/danmaku/settings";
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

  it("keeps lanes stable for millisecond rewinds instead of resetting on every step", () => {
    // Two rAF frames can report 8.05 then 8.04 (a paused sample, a rounding artefact); that is not a
    // seek. Resetting there would re-run lane assignment and make visible danmaku jump lanes.
    const entries = Array.from({ length: 60 }, (_, index) => entry(index, index * 0.4, "长".repeat(8)));
    const scheduler = createDanmakuScheduler(entries, { viewport, measure });

    const atHigh = scheduler.place(8.05);
    const atLow = scheduler.place(8.04);

    expect(atHigh.length).toBeGreaterThan(0);
    expect(atLow).toEqual(atHigh);
    expect(atLow.map((placement) => placement.lane)).toEqual(atHigh.map((placement) => placement.lane));

    // A rewind beyond the seek tolerance is a real seek and still recomputes lanes.
    const fresh = createDanmakuScheduler(entries, { viewport, measure });
    expect(scheduler.place(3)).toEqual(fresh.place(3));
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

describe("LuxPlayer danmaku layout settings", () => {
  const heightMeasure: MeasureText = (text, fontSize) => text.length * fontSize;

  function layout(overrides: Partial<DanmakuLayoutOptions> = {}): DanmakuLayoutOptions {
    return {
      ...DEFAULT_DANMAKU_LAYOUT,
      blockedModes: [],
      keywords: [],
      ...overrides,
    };
  }

  const scroll = entry(0, 0, "滚动");
  const top: LuxDanmakuEntry = { ...entry(1, 0, "顶部"), mode: "top" };
  const bottom: LuxDanmakuEntry = { ...entry(2, 0, "底部"), mode: "bottom" };

  it("hides blocked modes without disturbing the remaining lanes", () => {
    const scheduler = createDanmakuScheduler([scroll, top, bottom], {
      viewport,
      measure,
      layout: layout({ blockedModes: ["scroll"] }),
    });

    expect(scheduler.place(0.5).map((placement) => placement.mode).sort()).toEqual(["bottom", "top"]);
  });

  it("blocks case-insensitive plain substrings and never treats them as regular expressions", () => {
    const entries = [
      entry(0, 0, "Hello World"),
      entry(1, 0, "hello"),
      entry(2, 0, "再见"),
    ];
    const scheduler = createDanmakuScheduler(entries, {
      viewport,
      measure,
      layout: layout({ keywords: ["hello"] }),
    });

    // Second entry starts at the same time as the first, so only "再见" survives the filter.
    expect(scheduler.place(0.5).map((placement) => placement.text)).toEqual(["再见"]);

    expect(matchesDanmakuKeyword("Hello", ["hello"])).toBe(true);
    expect(matchesDanmakuKeyword("hello", ["h.llo"])).toBe(false);
    expect(matchesDanmakuKeyword("hello", [".*"])).toBe(false);
    expect(() => matchesDanmakuKeyword("hello", ["["])).not.toThrow();
  });

  it("scales the text size and measured width with the font multiplier", () => {
    const scheduler = createDanmakuScheduler([entry(0, 0, "12345")], {
      viewport,
      measure: heightMeasure,
      layout: layout({ fontScale: 2 }),
    });

    const placement = scheduler.place(0)[0]!;
    expect(placement.fontSize).toBe(50);
    expect(placement.width).toBe(250 + 8);
  });

  it("scales the scrolling crossing time with the speed multiplier", () => {
    const fast = createDanmakuScheduler([entry(0, 0)], {
      viewport,
      measure,
      layout: layout({ speed: 2 }),
    });
    const slow = createDanmakuScheduler([entry(0, 0)], {
      viewport,
      measure,
      layout: layout({ speed: 0.5 }),
    });

    expect(fast.place(0)[0]!.duration).toBe(SCROLL_DURATION_SECONDS / 2);
    expect(slow.place(0)[0]!.duration).toBe(SCROLL_DURATION_SECONDS / 0.5);
    // A faster crossing also means the entry leaves the screen sooner.
    expect(fast.place(SCROLL_DURATION_SECONDS / 2 + 0.1)).toEqual([]);
    expect(slow.place(SCROLL_DURATION_SECONDS / 2 + 0.1)).toHaveLength(1);
  });

  it("shows negative and positive time offsets at the expected media time", () => {
    const entryAtFive = entry(7, 5, "偏移");

    const early = createDanmakuScheduler([entryAtFive], {
      viewport,
      measure,
      layout: layout({ offsetSeconds: 2 }),
    });
    expect(early.place(2.9)).toEqual([]);
    expect(early.place(3).map((placement) => placement.text)).toEqual(["偏移"]);

    const late = createDanmakuScheduler([entryAtFive], {
      viewport,
      measure,
      layout: layout({ offsetSeconds: -2 }),
    });
    expect(late.place(5)).toEqual([]);
    expect(late.place(7).map((placement) => placement.text)).toEqual(["偏移"]);
  });

  it("restricts scrolling and top lanes to the selected vertical band", () => {
    const quarter = laneGeometry(720, "quarter");
    const full = laneGeometry(720, "full");
    expect(quarter.scrollLaneCount).toBeLessThan(full.scrollLaneCount);
    expect(quarter.fixedLaneCount).toBeLessThanOrEqual(full.fixedLaneCount);

    const scheduler = createDanmakuScheduler([scroll, top, bottom], {
      viewport: { width: 1280, height: 720 },
      measure,
      layout: layout({ area: "quarter" }),
    });
    const placements = scheduler.place(0.5);
    const band = 720 * 0.25;
    for (const placement of placements.filter((item) => item.mode !== "bottom")) {
      expect(placement.y).toBeLessThan(band);
      expect(placement.y).toBeGreaterThanOrEqual(0);
    }
    // Bottom danmaku are pinned to the video bottom and stay outside the band restriction.
    expect(placements.find((placement) => placement.mode === "bottom")!.y).toBeGreaterThan(720 / 2);
  });

  it("re-lanes the current window when settings change at runtime", () => {
    const entries = Array.from({ length: 40 }, (_, index) => entry(index, 0, "长".repeat(8)));
    const scheduler = createDanmakuScheduler(entries, { viewport, measure });

    const fullCount = scheduler.place(0.5).length;
    expect(fullCount).toBeGreaterThan(0);

    expect(scheduler.updateLayout({ area: "quarter" })).toBe(true);
    const quarter = scheduler.place(0.5);
    expect(quarter.length).toBeLessThan(fullCount);
    expect(quarter.every((placement) => placement.y < 450 * 0.25)).toBe(true);

    expect(scheduler.updateLayout({ blockedModes: ["scroll"] })).toBe(true);
    expect(scheduler.place(0.5)).toEqual([]);

    expect(scheduler.updateLayout({ blockedModes: [] })).toBe(true);
    const restored = scheduler.place(0.5);
    const fresh = createDanmakuScheduler(entries, {
      viewport,
      measure,
      layout: layout({ area: "quarter" }),
    }).place(0.5);
    expect(restored).toEqual(fresh);

    // No-op updates must not force another lane assignment.
    expect(scheduler.updateLayout({ area: "quarter", blockedModes: [] })).toBe(false);
  });

  it("applies a time offset change to the current window as well", () => {
    const scheduler = createDanmakuScheduler([entry(0, 5, "稍后")], { viewport, measure });
    expect(scheduler.place(0)).toEqual([]);

    scheduler.updateLayout({ offsetSeconds: 5 });
    expect(scheduler.place(0).map((placement) => placement.text)).toEqual(["稍后"]);
  });
});
