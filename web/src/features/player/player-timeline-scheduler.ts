export type PlaybackTimelineSnapshot = {
  currentTime: number;
  duration: number;
  bufferedEnd: number;
};

type FrameRequest = (callback: () => void) => number;
type FrameCancel = (frameId: number) => void;

export type PlaybackTimelineSchedulerOptions = {
  /** Minimum interval between non-critical React timeline updates. */
  minIntervalMs?: number;
  now?: () => number;
};

const defaultRequestFrame: FrameRequest = (callback) => {
  if (typeof globalThis.requestAnimationFrame === "function") {
    return globalThis.requestAnimationFrame(callback);
  }
  return globalThis.setTimeout(callback, 0) as unknown as number;
};

const defaultCancelFrame: FrameCancel = (frameId) => {
  if (typeof globalThis.cancelAnimationFrame === "function") {
    globalThis.cancelAnimationFrame(frameId);
  } else {
    globalThis.clearTimeout(frameId as unknown as ReturnType<typeof setTimeout>);
  }
};

/** Coalesces high-frequency media timeline events into one React update per frame. */
export function createPlaybackTimelineScheduler(
  onUpdate: (snapshot: PlaybackTimelineSnapshot) => void,
  requestFrame: FrameRequest = defaultRequestFrame,
  cancelFrame: FrameCancel = defaultCancelFrame,
  options: PlaybackTimelineSchedulerOptions = {},
) {
  const minIntervalMs = Math.max(0, options.minIntervalMs ?? 0);
  const now = options.now ?? (() => Date.now());
  let pending: PlaybackTimelineSnapshot | null = null;
  let frameId: number | null = null;
  let lastFlushAt = Number.NEGATIVE_INFINITY;

  const flush = (immediate = false) => {
    frameId = null;
    const snapshot = pending;
    if (!snapshot) return;
    if (!immediate && now() - lastFlushAt < minIntervalMs) return;
    pending = null;
    lastFlushAt = now();
    onUpdate(snapshot);
  };

  return {
    schedule(snapshot: PlaybackTimelineSnapshot, immediate = false) {
      pending = snapshot;
      if (immediate) {
        if (frameId !== null) cancelFrame(frameId);
        frameId = null;
        flush(true);
        return;
      }
      if (frameId === null && now() - lastFlushAt >= minIntervalMs) {
        frameId = requestFrame(flush);
      }
    },
    dispose() {
      if (frameId !== null) cancelFrame(frameId);
      frameId = null;
      pending = null;
    },
  };
}
