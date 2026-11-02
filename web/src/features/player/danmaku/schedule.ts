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
 *
 * User layout settings (`DanmakuLayoutOptions`) are applied here rather than in the renderer, because
 * blocking, the vertical display area, the text scale, the speed multiplier, and the time offset all
 * change which entries are visible and how lanes are assigned. `updateLayout` re-lanes the current
 * window in place, so changing a setting never reloads or re-parses the XML.
 */

import { DANMAKU_LIMITS, type DanmakuMode, type LuxDanmakuEntry } from "./parse";
import { createTextMeasurer, type MeasureText } from "./measure";
import {
  DANMAKU_SETTINGS_LIMITS,
  DEFAULT_DANMAKU_LAYOUT,
  danmakuLayoutsEqual,
  type DanmakuArea,
  type DanmakuLayoutOptions,
} from "./settings";

/**
 * Media-time tolerance shared with the renderer: a step beyond this is a seek (recompute lanes),
 * a smaller one is sampling jitter (keep the current lanes).
 */
export const DANMAKU_SEEK_THRESHOLD_SECONDS = 0.5;

export const SCROLL_DURATION_SECONDS = 8;
export const FIXED_DURATION_SECONDS = 4;
/** Horizontal breathing room between two scrolling danmaku sharing a lane. */
export const DANMAKU_LANE_GAP = 16;

/**
 * Bilibili's default danmaku font size. The lane pitch is derived from it (scaled by the user's font
 * multiplier), so lanes always reserve at least one full line even for files that only carry 18px
 * entries.
 */
export const DANMAKU_BASE_FONT_SIZE = 25;
/** Must match `line-height: 1.2` on `.lux-player-danmaku-text` in `player.css`. */
export const DANMAKU_LINE_HEIGHT_RATIO = 1.2;
/** Default pitch of one lane: 25px * 1.2 = 30px. */
export const DANMAKU_LANE_HEIGHT = Math.ceil(DANMAKU_BASE_FONT_SIZE * DANMAKU_LINE_HEIGHT_RATIO);

/** Fraction of the overlay height each display area may use for scrolling and top danmaku. */
export const DANMAKU_AREA_FRACTIONS: Record<DanmakuArea, number> = {
  full: 1,
  half: 0.5,
  quarter: 0.25,
};

/** Rendered font sizes stay legible after the user's scale without exploding memory/layout. */
const MIN_RENDERED_FONT_SIZE = 8;
const MAX_RENDERED_FONT_SIZE = 128;
const MODES: readonly DanmakuMode[] = ["scroll", "top", "bottom"];

export type DanmakuViewport = { width: number; height: number };

export type ScheduledDanmaku = LuxDanmakuEntry & {
  lane: number;
  duration: number;
  width: number;
  /** Vertical pitch reserved for this entry's lane; adjacent lanes start at least this far apart. */
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
  /**
   * Applies new layout settings and re-lanes the current window. Returns `true` when the settings
   * actually changed (a no-op update does not disturb the lanes already on screen).
   *
   * Only the discrete block/keyword edits rebuild the filtered table; slider changes (font scale,
   * speed, area, offset) reuse it, so dragging a slider over a 50k-entry file stays O(1) in the
   * number of entries.
   */
  updateLayout(next: Partial<DanmakuLayoutOptions>): boolean;
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
  /** Initial user layout settings; `updateLayout` can replace them at runtime. */
  layout?: Partial<DanmakuLayoutOptions>;
  /** Test/diagnostic hook: fires whenever the filtered entry table is rebuilt. */
  onPrepare?: () => void;
};

/**
 * Lane geometry for a viewport height and a display area.
 *
 * `lineHeight` is the uniform pitch (already scaled by the user's font multiplier) shared by every
 * lane; the fixed band, scrolling lanes, and top lanes all derive from it so `laneY` can never
 * disagree with the lane count. The geometry is also clamped by the available band, so a small
 * overlay or a `half`/`quarter` area never places a lane outside the selected band. The band may be
 * too short for any lane (the count becomes 0), which is the honest answer instead of drawing over
 * the allowed area.
 */
export function laneGeometry(
  height: number,
  area: DanmakuArea = "full",
  lineHeight: number = DANMAKU_LANE_HEIGHT,
) {
  const safeHeight = Math.max(1, height);
  const pitch = Math.max(1, Math.ceil(lineHeight));
  const bandHeight = Math.max(1, safeHeight * DANMAKU_AREA_FRACTIONS[area]);
  // Fixed (top) lanes: two tops with a 16%-of-band target, but never more than the band can hold.
  const fixedCapacity = Math.floor((bandHeight - 8) / pitch);
  const fixedLaneCount = Math.max(
    0,
    Math.min(4, fixedCapacity, Math.max(1, Math.floor((bandHeight * 0.16) / pitch))),
  );
  const fixedBandHeight = fixedLaneCount * pitch + 8;
  // Scrolling lanes sit below the fixed band and must end before the band's bottom edge.
  const scrollCapacity = Math.floor((bandHeight - fixedBandHeight - 8) / pitch);
  const desiredScrollHeight = Math.max(pitch * 4, bandHeight - fixedBandHeight * 2);
  const scrollLaneCount = Math.max(
    0,
    Math.min(
      scrollCapacity,
      Math.max(4, Math.min(18, Math.floor((desiredScrollHeight * 0.64) / pitch))),
    ),
  );
  return { fixedLaneCount, fixedBandHeight, scrollLaneCount, lineHeight: pitch };
}

function durationFor(mode: DanmakuMode, speed: number) {
  // The speed multiplier scales the whole danmaku lifetime: scrolling crossing time and the dwell
  // time of top/bottom danmaku alike.
  const base = mode === "scroll" ? SCROLL_DURATION_SECONDS : FIXED_DURATION_SECONDS;
  return base / speed;
}

function laneCountFor(
  mode: DanmakuMode,
  areaGeometry: ReturnType<typeof laneGeometry>,
  fullGeometry: ReturnType<typeof laneGeometry>,
) {
  if (mode === "scroll") return areaGeometry.scrollLaneCount;
  // Bottom danmaku ignore the display-area restriction; top danmaku honour it.
  return mode === "top" ? areaGeometry.fixedLaneCount : fullGeometry.fixedLaneCount;
}

function laneY(
  mode: DanmakuMode,
  lane: number,
  viewport: DanmakuViewport,
  areaGeometry: ReturnType<typeof laneGeometry>,
  pitch: number,
) {
  // `pitch` is the scheduled entry's own `lineHeight`, so the lane pitch used for `y` can never
  // drift from the one reserved in `ScheduledDanmaku`.
  if (mode === "top") return lane * pitch + 8;
  if (mode === "bottom") return viewport.height - (lane + 1) * pitch - 8;
  return areaGeometry.fixedBandHeight + lane * pitch + 8;
}

/** Case-insensitive plain substring match. Keywords are data, never a regular expression. */
export function matchesDanmakuKeyword(text: string, keywords: readonly string[]): boolean {
  if (keywords.length === 0) return false;
  const haystack = text.toLowerCase();
  return keywords.some((keyword) => haystack.includes(keyword));
}

export function isDanmakuEntryHidden(
  entry: LuxDanmakuEntry,
  layout: DanmakuLayoutOptions,
): boolean {
  return layout.blockedModes.includes(entry.mode)
    || matchesDanmakuKeyword(entry.text, layout.keywords);
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

function clampNumber(value: unknown, min: number, max: number, fallback: number) {
  if (typeof value !== "number" || !Number.isFinite(value)) return fallback;
  return Math.min(max, Math.max(min, value));
}

function sameList<T>(left: readonly T[], right: readonly T[]): boolean {
  return left.length === right.length && left.every((value, index) => value === right[index]);
}

/**
 * Defensive normalization of layout input. The settings module already normalizes, but the scheduler
 * is also usable from tests and future call sites, so it never trusts the values it receives.
 */
function resolveLayout(input: Partial<DanmakuLayoutOptions> = {}): DanmakuLayoutOptions {
  const area = input.area === "half" || input.area === "quarter" || input.area === "full"
    ? input.area
    : DEFAULT_DANMAKU_LAYOUT.area;
  const blockedModes = [...new Set(input.blockedModes ?? [])]
    .filter((mode): mode is DanmakuMode => MODES.includes(mode));
  const keywords = [
    ...new Set(
      (input.keywords ?? [])
        .filter((keyword): keyword is string => typeof keyword === "string" && keyword.length > 0)
        .map((keyword) => keyword.toLowerCase()),
    ),
  ];
  return {
    area,
    fontScale: clampNumber(
      input.fontScale,
      DANMAKU_SETTINGS_LIMITS.fontScaleMin,
      DANMAKU_SETTINGS_LIMITS.fontScaleMax,
      DEFAULT_DANMAKU_LAYOUT.fontScale,
    ),
    speed: clampNumber(
      input.speed,
      DANMAKU_SETTINGS_LIMITS.speedMin,
      DANMAKU_SETTINGS_LIMITS.speedMax,
      DEFAULT_DANMAKU_LAYOUT.speed,
    ),
    offsetSeconds: clampNumber(
      input.offsetSeconds,
      DANMAKU_SETTINGS_LIMITS.offsetMin,
      DANMAKU_SETTINGS_LIMITS.offsetMax,
      DEFAULT_DANMAKU_LAYOUT.offsetSeconds,
    ),
    blockedModes,
    keywords,
  };
}

export function createDanmakuScheduler(
  entries: readonly LuxDanmakuEntry[],
  options: DanmakuSchedulerOptions,
): DanmakuScheduler {
  // Entries arrive sorted by start from the parser; sorting a copy keeps callers free to pass
  // anything (and keeps `place()` cheap).
  const sourceEntries = [...entries].sort((left, right) => left.start - right.start);
  const measure = options.measure ?? createTextMeasurer();
  const maxVisible = options.maxVisible ?? DANMAKU_LIMITS.maxVisible;
  const gap = options.gap ?? DANMAKU_LANE_GAP;
  const onPrepare = options.onPrepare;
  let viewport: DanmakuViewport = {
    width: Math.max(1, options.viewport.width),
    height: Math.max(1, options.viewport.height),
  };
  let layout = resolveLayout(options.layout);
  let prepared: LuxDanmakuEntry[] = [];
  let maxSourceFontSize = 0;
  // One pitch for every lane: the largest rendered entry (at least the base 25px) times the CSS
  // line-height. This is what makes `ScheduledDanmaku.lineHeight` the real vertical reservation.
  let laneHeight = DANMAKU_LANE_HEIGHT;
  let areaGeometry = laneGeometry(viewport.height, layout.area, laneHeight);
  let fullGeometry = laneGeometry(viewport.height, "full", laneHeight);
  // Media clock of the last `place()`, already shifted by the user's time offset.
  let lastTime = Number.NEGATIVE_INFINITY;
  let nextIndex = 0;
  let live: ScheduledDanmaku[] = [];
  const lanes: Record<DanmakuMode, Array<ScheduledDanmaku | undefined>> = {
    scroll: [],
    top: [],
    bottom: [],
  };

  function computeLaneHeight() {
    const baseFontSize = Math.max(DANMAKU_BASE_FONT_SIZE, maxSourceFontSize);
    laneHeight = Math.max(1, Math.ceil(baseFontSize * layout.fontScale * DANMAKU_LINE_HEIGHT_RATIO));
  }

  function prepare() {
    prepared = sourceEntries.filter((entry) => !isDanmakuEntryHidden(entry, layout));
    maxSourceFontSize = 0;
    for (const entry of prepared) {
      if (entry.fontSize > maxSourceFontSize) maxSourceFontSize = entry.fontSize;
    }
    computeLaneHeight();
    onPrepare?.();
  }

  function applyViewport() {
    areaGeometry = laneGeometry(viewport.height, layout.area, laneHeight);
    fullGeometry = laneGeometry(viewport.height, "full", laneHeight);
    lanes.scroll = Array.from({ length: areaGeometry.scrollLaneCount });
    lanes.top = Array.from({ length: areaGeometry.fixedLaneCount });
    lanes.bottom = Array.from({ length: fullGeometry.fixedLaneCount });
  }

  function reset(mediaTime: number) {
    const longestLifetime = SCROLL_DURATION_SECONDS / layout.speed;
    nextIndex = lowerBoundByStart(prepared, mediaTime - longestLifetime);
    live = [];
    applyViewport();
    lastTime = mediaTime;
  }

  function renderedFontSize(entry: LuxDanmakuEntry) {
    return Math.max(
      MIN_RENDERED_FONT_SIZE,
      Math.min(MAX_RENDERED_FONT_SIZE, Math.round(entry.fontSize * layout.fontScale)),
    );
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
    const duration = durationFor(source.mode, layout.speed);
    const fontSize = renderedFontSize(source);
    const width = Math.min(
      viewport.width * 0.9,
      Math.max(24, measure(source.text, fontSize) + 8),
    );
    const candidate: ScheduledDanmaku = {
      ...source,
      fontSize,
      lane: 0,
      duration,
      width,
      lineHeight: laneHeight,
      y: 0,
    };
    const modeLanes = lanes[source.mode];
    const count = laneCountFor(source.mode, areaGeometry, fullGeometry);
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
    candidate.y = laneY(source.mode, lane, viewport, areaGeometry, candidate.lineHeight);
    modeLanes[lane] = candidate;
    live.push(candidate);
  }

  function place(requestedTime: number): DanmakuPlacement[] {
    if (!Number.isFinite(requestedTime) || requestedTime < 0) return [];
    // The user's offset is applied to the clock instead of rewriting the prepared table: a positive
    // offset shows danmaku earlier, a negative one later, and the start ordering is untouched.
    let mediaTime = requestedTime + layout.offsetSeconds;
    // A gap beyond the longest danmaku lifetime or a real rewind invalidates the lane state. A
    // sub-threshold rewind is sampling jitter (two frames can report 8.05 then 8.04), so it keeps
    // the current lanes and is clamped instead of re-assigning lanes under the viewer's eyes.
    if (mediaTime > lastTime + SCROLL_DURATION_SECONDS / layout.speed
      || mediaTime < lastTime - DANMAKU_SEEK_THRESHOLD_SECONDS) {
      reset(mediaTime);
    } else if (mediaTime < lastTime) {
      mediaTime = lastTime;
    }
    lastTime = mediaTime;

    while (nextIndex < prepared.length && prepared[nextIndex]!.start <= mediaTime) {
      schedule(prepared[nextIndex]!);
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

  prepare();
  reset(0);

  return {
    configure(next) {
      viewport = { width: Math.max(1, next.width), height: Math.max(1, next.height) };
      lastTime = Number.NEGATIVE_INFINITY;
    },
    updateLayout(next) {
      const merged = resolveLayout({ ...layout, ...next });
      if (danmakuLayoutsEqual(merged, layout)) return false;
      const filtersChanged = !sameList(merged.blockedModes, layout.blockedModes)
        || !sameList(merged.keywords, layout.keywords);
      layout = merged;
      if (filtersChanged) {
        // Discrete edits (mode switches, keyword add/remove) rebuild the filtered table once.
        prepare();
      } else {
        // Slider edits reuse the filtered table; only the O(1) pitch needs recomputing.
        computeLaneHeight();
      }
      // Filtering, the display area, text scale, speed, and offset all affect lane assignment, so
      // the whole current window is re-laned instead of keeping stale placements. The offset itself
      // is applied in `place()` and never rewrites the table.
      lastTime = Number.NEGATIVE_INFINITY;
      return true;
    },
    resync() {
      lastTime = Number.NEGATIVE_INFINITY;
    },
    place,
  };
}
