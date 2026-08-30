export type PlaybackTimelineSnapshot = {
  currentTime: number;
  duration: number;
  bufferedEnd: number;
};

type FrameRequest = (callback: () => void) => number;
type FrameCancel = (frameId: number) => void;

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
) {
  let pending: PlaybackTimelineSnapshot | null = null;
  let frameId: number | null = null;

  const flush = () => {
    frameId = null;
    const snapshot = pending;
    pending = null;
    if (snapshot) onUpdate(snapshot);
  };

  return {
    schedule(snapshot: PlaybackTimelineSnapshot, immediate = false) {
      pending = snapshot;
      if (immediate) {
        if (frameId !== null) cancelFrame(frameId);
        flush();
        return;
      }
      if (frameId === null) frameId = requestFrame(flush);
    },
    dispose() {
      if (frameId !== null) cancelFrame(frameId);
      frameId = null;
      pending = null;
    },
  };
}
