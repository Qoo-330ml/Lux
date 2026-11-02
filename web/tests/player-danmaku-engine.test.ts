// @vitest-environment jsdom

import { describe, expect, it } from "vitest";
import {
  DANMAKU_SEEK_THRESHOLD_SECONDS,
  createDanmakuEngine,
  createDanmakuScheduler,
  interpolateMediaTime,
  type DanmakuScheduler,
  type MeasureText,
} from "../src/features/player/danmaku";
import type { LuxDanmakuEntry } from "../src/features/player/danmaku";

const viewport = { width: 800, height: 450 };
const measure: MeasureText = (text) => text.length * 20;

function entry(index: number, start: number, text = "弹幕"): LuxDanmakuEntry {
  return { id: `danmaku-${index}`, start, mode: "scroll", text, color: "#ffffff", fontSize: 25 };
}

function createFrameDriver() {
  let wallClock = 0;
  const pending: Array<FrameRequestCallback> = [];
  return {
    requestFrame: (callback: FrameRequestCallback) => {
      pending.push(callback);
      return pending.length;
    },
    cancelFrame: () => pending.splice(0, pending.length),
    now: () => wallClock,
    /** Advance wall-clock time and run the frame the engine asked for. */
    step(milliseconds: number) {
      wallClock += milliseconds;
      const callback = pending.shift();
      callback?.(wallClock);
    },
    pendingCount: () => pending.length,
  };
}

function createHarness(entries: LuxDanmakuEntry[] = [entry(0, 0, "12345")]) {
  const container = document.createElement("div");
  document.body.append(container);
  let scheduler: DanmakuScheduler = createDanmakuScheduler(entries, { viewport, measure });
  const driver = createFrameDriver();
  const engine = createDanmakuEngine({
    container,
    scheduler: () => scheduler,
    requestFrame: driver.requestFrame,
    cancelFrame: driver.cancelFrame,
    now: driver.now,
  });
  return {
    container,
    engine,
    driver,
    reload: (next: LuxDanmakuEntry[]) => {
      scheduler = createDanmakuScheduler(next, { viewport, measure });
    },
  };
}

function nodes(container: HTMLElement) {
  return Array.from(container.querySelectorAll<HTMLElement>(".lux-player-danmaku-text"));
}

describe("LuxPlayer danmaku frame clock", () => {
  it("advances media time with the playback rate and freezes while paused", () => {
    const anchor = { mediaTime: 10, wallTime: 1_000, playing: true, playbackRate: 1 };
    expect(interpolateMediaTime(anchor, 2_000)).toBeCloseTo(11, 6);
    expect(interpolateMediaTime({ ...anchor, playbackRate: 2 }, 2_000)).toBeCloseTo(12, 6);
    expect(interpolateMediaTime({ ...anchor, playbackRate: 0.5 }, 2_000)).toBeCloseTo(10.5, 6);
    expect(interpolateMediaTime({ ...anchor, playing: false, playbackRate: 2 }, 9_000)).toBe(10);
    expect(interpolateMediaTime({ ...anchor, playbackRate: 2 }, 500)).toBe(10);
    expect(interpolateMediaTime({ ...anchor, mediaTime: Number.NaN }, 2_000)).toBe(0);
  });
});

describe("LuxPlayer danmaku renderer", () => {
  it("drives positions with transform instead of re-rendering on every frame", () => {
    const { container, engine, driver } = createHarness();
    engine.sync({ mediaTime: 0, playing: true, playbackRate: 1 });

    const [node] = nodes(container);
    expect(node).toBeDefined();
    expect(node!.textContent).toBe("12345");
    expect(node!.children).toHaveLength(0);
    expect(node!.style.transform).toContain("translate3d");
    expect(node!.style.left).toBe("");
    expect(node!.style.top).toBe("");
    // 5 characters at 20px plus the shared padding, capped by the overlay width.
    expect(node!.style.transform).toBe("translate3d(800px, 84px, 0)");

    driver.step(1_000);
    const [moved] = nodes(container);
    // The DOM node is reused; only its transform changed one second later.
    expect(moved).toBe(node);
    expect(moved!.style.transform).toBe("translate3d(686.5px, 84px, 0)");
  });

  it("scrolls twice as fast at 2x playback by advancing the media-time clock", () => {
    const { container, engine, driver } = createHarness();
    engine.sync({ mediaTime: 0, playing: true, playbackRate: 2 });

    driver.step(1_000);
    const [node] = nodes(container);
    // One wall-clock second at 2x equals two media seconds, matching the video rhythm.
    expect(node!.style.transform).toBe("translate3d(573px, 84px, 0)");
  });

  it("stops rendering frames while paused and resumes from the new anchor", () => {
    const { container, engine, driver } = createHarness();
    engine.sync({ mediaTime: 0, playing: true, playbackRate: 1 });
    driver.step(1_000);
    const paused = nodes(container)[0]!.style.transform;

    engine.sync({ mediaTime: 1, playing: false, playbackRate: 1 });
    expect(driver.pendingCount()).toBe(0);
    driver.step(5_000);
    expect(nodes(container)[0]!.style.transform).toBe(paused);

    engine.sync({ mediaTime: 5, playing: true, playbackRate: 1 });
    expect(nodes(container)[0]!.style.transform).toBe("translate3d(232.5px, 84px, 0)");
  });

  it("re-anchors immediately after a seek instead of interpolating across the jump", () => {
    const entries = [entry(0, 0, "12345"), entry(1, 30, "67890")];
    const { container, engine, driver } = createHarness(entries);
    engine.sync({ mediaTime: 0, playing: true, playbackRate: 1 });
    driver.step(1_000);

    engine.sync({ mediaTime: 30, playing: true, playbackRate: 1 });
    const rendered = nodes(container).map((node) => node.dataset.danmakuId);
    expect(rendered).toEqual(["danmaku-1"]);
    expect(rendered.length).toBeGreaterThan(0);
    expect(DANMAKU_SEEK_THRESHOLD_SECONDS).toBeLessThan(5);
  });

  it("removes nodes when danmaku leaves the screen and on destroy", () => {
    const { container, engine, driver } = createHarness();
    engine.sync({ mediaTime: 0, playing: true, playbackRate: 1 });
    expect(nodes(container)).toHaveLength(1);

    driver.step(8_000);
    expect(nodes(container)).toHaveLength(0);

    engine.sync({ mediaTime: 0, playing: true, playbackRate: 1 });
    expect(nodes(container)).toHaveLength(1);
    engine.destroy();
    expect(nodes(container)).toHaveLength(0);
    expect(driver.pendingCount()).toBe(0);
  });

  it("drops stale nodes when the track is reloaded with different entries", () => {
    // Entry ids restart at `danmaku-0` on every parse, so a rebuild must not reuse the old node and
    // keep showing the previous file's text.
    const { container, engine, reload } = createHarness([entry(0, 0, "旧弹幕")]);
    engine.sync({ mediaTime: 0, playing: false, playbackRate: 1 });
    expect(nodes(container)[0]!.textContent).toBe("旧弹幕");

    reload([entry(0, 0, "新弹幕")]);
    engine.sync({ mediaTime: 0, playing: false, playbackRate: 1 });

    expect(nodes(container)).toHaveLength(1);
    expect(nodes(container)[0]!.textContent).toBe("新弹幕");
  });

  it("renders hostile text as text nodes, never as markup", () => {
    const { container, engine } = createHarness([entry(0, 0, "<img src=x onerror=alert(1)>")]);
    engine.sync({ mediaTime: 0, playing: false, playbackRate: 1 });

    const [node] = nodes(container);
    expect(node!.textContent).toBe("<img src=x onerror=alert(1)>");
    expect(node!.querySelector("img")).toBeNull();
    expect(node!.classList.contains("lux-player-danmaku-scroll")).toBe(true);
    expect(node!.dataset.danmakuId).toBe("danmaku-0");
    expect(node!.style.color).toBe("rgb(255, 255, 255)");
    expect(node!.style.fontSize).toBe("25px");
  });
});
