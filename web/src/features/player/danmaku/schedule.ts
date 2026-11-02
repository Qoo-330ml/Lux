/**
 * Danmaku lane scheduling.
 *
 * The scheduler assigns lanes incrementally from the current playback window instead of
 * pre-computing a static layout for the whole file, so jumping to another position (seek, chapter
 * skip, resume) recomputes lanes for that window rather than reusing stale assignments. Lane fit uses
 * the measured text width (`measure.ts`) and checks both spawn clearance and catch-up, so scrolling
 * danmaku in one lane can never overlap or run into the entry ahead of it.
 *
 * All times here are media (video) seconds: the renderer advances this clock with `playbackRate`
 * (`engine.ts`), which keeps scrolling speed in step with the video rhythm at any playback rate.
 */

import { DANMAKU_LIMITS, type DanmakuMode, type LuxDanmakuEntry } from "./parse";
import { createTextMeasurer, type MeasureText } from "./measure";

/**
 * Media-time tolerance shared with the renderer: a step beyond this is a seek (recompute lanes),
 * a smaller one is sampling jitter (keep the current lanes).
 */
export const DANMAKU_SEEK_THRESHOLD_SECONDS = 0.5;

export const SCROLL_DURATION_SECONDS = 8;
export const FIXED_DURATION_SECONDS = 4;
export const SCROLL_LINE_HEIGHT = 32;
export const FIXED_LINE_HEIGHT = 34;
/** Horizontal breathing room between two scrolling danmaku sharing a lane. */
export const DANMAKU_LANE_GAP = 16;

export type DanmakuViewport = { width: number; height: number };

export type ScheduledDanmaku = LuxDanmakuEntry & {
  lane: number;
  duration: number;
  width: number;
  lineHeight: number;
  y: number;
};

export type DanmakuPlacement = ScheduledDanmaku & {
  x: number;
  progress: number;
};

export type DanmakuScheduler = {
  /** Applies a new viewport; the next `place()` recomputes lanes for the current window. */
  configure(viewport: DanmakuViewport): void;
  /** Drops the incremental lane state (used after a seek or a source change). */
  resync(): void;
  /** Returns the danmaku visible at `mediaTime`, with positions for that instant. */
  place(mediaTime: number): DanmakuPlacement[];
};

export type DanmakuSchedulerOptions = {
  viewport: DanmakuViewport;
  /** Injected in tests; defaults to a cached canvas measurer. */
  measure?: MeasureText;
  maxVisible?: number;
  gap?: number;
};

export function laneGeometry(height: number) {
  const safeHeight = Math.max(1, height);
  const fixedLaneCount = Math.max(2, Math.min(4, Math.floor((safeHeight * 0.16) / FIXED_LINE_HEIGHT)));
  const fixedBandHeight = fixedLaneCount * FIXED_LINE_HEIGHT + 8;
  const availableScrollHeight = Math.max(SCROLL_LINE_HEIGHT * 4, safeHeight - fixedBandHeight * 2);
  const scrollLaneCount = Math.max(
    4,
    Math.min(18, Math.floor((availableScrollHeight * 0.64) / SCROLL_LINE_HEIGHT)),
  );
  return { fixedLaneCount, fixedBandHeight, scrollLaneCount };
}

function durationFor(mode: DanmakuMode) {
  return mode === "scroll" ? SCROLL_DURATION_SECONDS : FIXED_DURATION_SECONDS;
}

function laneCountFor(mode: DanmakuMode, geometry: ReturnType<typeof laneGeometry>) {
  return mode === "scroll" ? geometry.scrollLaneCount : geometry.fixedLaneCount;
}

function laneY(mode: DanmakuMode, lane: number, viewport: DanmakuViewport, geometry: ReturnType<typeof laneGeometry>) {
  if (mode === "top") return lane * FIXED_LINE_HEIGHT + 8;
  if (mode === "bottom") return viewport.height - (lane + 1) * FIXED_LINE_HEIGHT - 8;
  return geometry.fixedBandHeight + lane * SCROLL_LINE_HEIGHT + 8;
}

/** First index whose start is strictly greater than `time`. */
function lowerBoundByStart(entries: readonly LuxDanmakuEntry[], time: number) {
  let low = 0;
  let high = entries.length;
  while (low < high) {
    const mid = (low + high) >> 1;
    if (entries[mid]!.start <= time) low = mid + 1;
    else high = mid;
  }
  return low;
}

export function createDanmakuScheduler(
  entries: readonly LuxDanmakuEntry[],
  options: DanmakuSchedulerOptions,
): DanmakuScheduler {
  // Entries arrive sorted by start from the parser; sorting a copy keeps callers free to pass
  // anything (and keeps `place()` cheap).
  const sorted = [...entries].sort((left, right) => left.start - right.start);
  const measure = options.measure ?? createTextMeasurer();
  const maxVisible = options.maxVisible ?? DANMAKU_LIMITS.maxVisible;
  const gap = options.gap ?? DANMAKU_LANE_GAP;
  let viewport: DanmakuViewport = {
    width: Math.max(1, options.viewport.width),
    height: Math.max(1, options.viewport.height),
  };
  let lastTime = Number.NEGATIVE_INFINITY;
  let nextIndex = 0;
  let live: ScheduledDanmaku[] = [];
  const lanes: Record<DanmakuMode, Array<ScheduledDanmaku | undefined>> = {
    scroll: [],
    top: [],
    bottom: [],
  };

  function applyViewport() {
    const geometry = laneGeometry(viewport.height);
    lanes.scroll = Array.from({ length: geometry.scrollLaneCount });
    lanes.top = Array.from({ length: geometry.fixedLaneCount });
    lanes.bottom = Array.from({ length: geometry.fixedLaneCount });
  }

  function reset(mediaTime: number) {
    nextIndex = lowerBoundByStart(sorted, mediaTime - SCROLL_DURATION_SECONDS);
    live = [];
    applyViewport();
    lastTime = mediaTime;
  }

  function fits(previous: ScheduledDanmaku | undefined, entry: ScheduledDanmaku) {
    if (!previous) return true;
    if (previous.start + previous.duration <= entry.start) return true;
    if (entry.mode !== "scroll") return false;
    // The previous danmaku must be far enough left that the new one enters behind it...
    const elapsed = (entry.start - previous.start) / previous.duration;
    const previousRightAtSpawn = viewport.width - elapsed * (viewport.width + previous.width) + previous.width;
    if (previousRightAtSpawn > viewport.width - gap) return false;
    // ...and it must stay behind until the previous one has fully left the screen, otherwise a
    // wider (faster) entry would catch up and overlap mid-lane.
    const newXWhenPreviousExits = viewport.width
      - ((previous.start + previous.duration - entry.start) / entry.duration)
      * (viewport.width + entry.width);
    return newXWhenPreviousExits >= gap;
  }

  function schedule(source: LuxDanmakuEntry) {
    const geometry = laneGeometry(viewport.height);
    const duration = durationFor(source.mode);
    const width = Math.min(
      viewport.width * 0.9,
      Math.max(24, measure(source.text, source.fontSize) + 8),
    );
    const candidate: ScheduledDanmaku = {
      ...source,
      lane: 0,
      duration,
      width,
      lineHeight: source.mode === "scroll" ? SCROLL_LINE_HEIGHT : FIXED_LINE_HEIGHT,
      y: 0,
    };
    const modeLanes = lanes[source.mode];
    const count = laneCountFor(source.mode, geometry);
    let lane = -1;
    for (let index = 0; index < count && index < modeLanes.length; index += 1) {
      if (fits(modeLanes[index], candidate)) {
        lane = index;
        break;
      }
    }
    // No free lane: drop this danmaku rather than stacking it on top of another one.
    if (lane < 0) return;
    candidate.lane = lane;
    candidate.y = laneY(source.mode, lane, viewport, geometry);
    modeLanes[lane] = candidate;
    live.push(candidate);
  }

  function place(requestedTime: number): DanmakuPlacement[] {
    if (!Number.isFinite(requestedTime) || requestedTime < 0) return [];
    let mediaTime = requestedTime;
    // A gap beyond the longest danmaku lifetime or a real rewind invalidates the lane state. A
    // sub-threshold rewind is sampling jitter (two frames can report 8.05 then 8.04), so it keeps
    // the current lanes and is clamped instead of re-assigning lanes under the viewer's eyes.
    if (mediaTime > lastTime + SCROLL_DURATION_SECONDS
      || mediaTime < lastTime - DANMAKU_SEEK_THRESHOLD_SECONDS) {
      reset(mediaTime);
    } else if (mediaTime < lastTime) {
      mediaTime = lastTime;
    }
    lastTime = mediaTime;

    while (nextIndex < sorted.length && sorted[nextIndex]!.start <= mediaTime) {
      schedule(sorted[nextIndex]!);
      nextIndex += 1;
    }
    live = live.filter((entry) => entry.start + entry.duration > mediaTime);

    const visible: DanmakuPlacement[] = [];
    for (const entry of live) {
      if (entry.start > mediaTime) continue;
      if (visible.length >= maxVisible) break;
      const progress = Math.max(0, Math.min(1, (mediaTime - entry.start) / entry.duration));
      const x = entry.mode === "scroll"
        ? viewport.width - progress * (viewport.width + entry.width)
        : (viewport.width - entry.width) / 2;
      visible.push({ ...entry, x, progress });
    }
    return visible.sort((left, right) => left.start - right.start);
  }

  reset(0);

  return {
    configure(next) {
      viewport = { width: Math.max(1, next.width), height: Math.max(1, next.height) };
      lastTime = Number.NEGATIVE_INFINITY;
    },
    resync() {
      lastTime = Number.NEGATIVE_INFINITY;
    },
    place,
  };
}
