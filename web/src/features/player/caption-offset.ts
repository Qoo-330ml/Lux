import type { LuxCaptionCue } from "./caption-parser";

export const CAPTION_OFFSET_MIN = -10;
export const CAPTION_OFFSET_MAX = 10;
export const CAPTION_OFFSET_STEP = 0.1;

export type CaptionTimeRange = {
  start: number;
  end: number;
};

export function normalizeCaptionOffset(value: number) {
  if (!Number.isFinite(value)) return 0;
  const bounded = Math.min(CAPTION_OFFSET_MAX, Math.max(CAPTION_OFFSET_MIN, value));
  return Number((Math.round(bounded / CAPTION_OFFSET_STEP) * CAPTION_OFFSET_STEP).toFixed(1));
}

export function formatCaptionOffset(value: number) {
  const normalized = normalizeCaptionOffset(value);
  return `${normalized > 0 ? "+" : ""}${normalized.toFixed(1)}s`;
}

export function offsetCaptionTimeRange(
  range: CaptionTimeRange,
  offset: number,
  duration?: number | null,
): CaptionTimeRange | null {
  if (!Number.isFinite(range.start) || !Number.isFinite(range.end) || range.end <= range.start) {
    return null;
  }
  const maximum = Number.isFinite(duration) && (duration ?? 0) > 0
    ? Math.max(0, duration ?? 0)
    : Number.POSITIVE_INFINITY;
  const start = Math.max(0, Math.min(maximum, range.start + normalizeCaptionOffset(offset)));
  const end = Math.max(0, Math.min(maximum, range.end + normalizeCaptionOffset(offset)));
  return end > start ? { start, end } : null;
}

export function offsetCaptionCues(
  cues: readonly LuxCaptionCue[],
  offset: number,
  duration?: number | null,
) {
  return cues.flatMap((cue) => {
    const range = offsetCaptionTimeRange(cue, offset, duration);
    return range ? [{ ...cue, ...range }] : [];
  });
}

type NativeCaptionCueRecord = {
  cue: TextTrackCue;
  original: CaptionTimeRange;
  removed: boolean;
};

/**
 * Keeps browser-owned WebVTT cues tied to their original timestamps. The
 * track is mutated only for the active player generation and can be restored
 * when the selected caption, source, or engine changes.
 */
export function createNativeCaptionOffsetController(track: TextTrack) {
  const records = new Map<TextTrackCue, NativeCaptionCueRecord>();

  const remember = (cue: TextTrackCue) => {
    if (!records.has(cue)) {
      records.set(cue, {
        cue,
        original: { start: cue.startTime, end: cue.endTime },
        removed: false,
      });
    }
    return records.get(cue)!;
  };

  const apply = (offset: number, duration?: number | null) => {
    const current = nativeCues(track);
    current.forEach(remember);

    for (const record of records.values()) {
      const next = offsetCaptionTimeRange(record.original, offset, duration);
      if (!next) {
        if (!record.removed && typeof track.removeCue === "function" && nativeCues(track).includes(record.cue)) {
          try {
            track.removeCue(record.cue);
            record.removed = true;
          } catch {
            // A browser may reject removal while the track is being replaced.
          }
        }
        continue;
      }

      if (record.removed && typeof track.addCue === "function") {
        try {
          track.addCue(record.cue);
          record.removed = false;
        } catch {
          continue;
        }
      }
      if (!record.removed) setCueTiming(record.cue, next);
    }
  };

  const restore = () => {
    for (const record of records.values()) {
      if (record.removed && typeof track.addCue === "function") {
        try {
          track.addCue(record.cue);
          record.removed = false;
        } catch {
          continue;
        }
      }
      if (!record.removed && nativeCues(track).includes(record.cue)) {
        setCueTiming(record.cue, record.original);
      }
    }
    records.clear();
  };

  return { apply, restore };
}

function nativeCues(track: TextTrack) {
  const list = track.cues;
  if (!list) return [];
  const cues: TextTrackCue[] = [];
  for (let index = 0; index < list.length; index += 1) {
    const cue = list[index];
    if (cue) cues.push(cue);
  }
  return cues;
}

function setCueTiming(cue: TextTrackCue, range: CaptionTimeRange) {
  try {
    if (range.start <= cue.endTime) cue.startTime = range.start;
    cue.endTime = range.end;
    if (cue.startTime !== range.start) cue.startTime = range.start;
  } catch {
    // Invalid timing is ignored by the browser rather than breaking playback.
  }
}
