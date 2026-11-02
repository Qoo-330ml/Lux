/**
 * Danmaku rendering engine.
 *
 * React only mounts the host element; this module owns the danmaku DOM nodes and moves them with
 * `transform: translate3d(...)` on every animation frame. Positions are interpolated from the last
 * media time React reported and the playback rate, so the 4-times-per-second `timeupdate` cadence no
 * longer drives layout: React re-renders only when danmaku enter or leave the track.
 *
 * Media time (video seconds) is the clock, and it advances `playbackRate` seconds per wall-clock
 * second, so scrolling speed stays in step with the video at any playback rate.
 */

import type { DanmakuPlacement, DanmakuScheduler } from "./schedule";

/** A media-time jump bigger than this is a seek, not interpolation drift. */
export const DANMAKU_SEEK_THRESHOLD_SECONDS = 0.5;

export type DanmakuClockAnchor = {
  mediaTime: number;
  wallTime: number;
  playing: boolean;
  playbackRate: number;
};

export type DanmakuClockState = {
  mediaTime: number;
  playing: boolean;
  playbackRate: number;
};

export type DanmakuEngineOptions = {
  container: HTMLElement;
  /** Resolved per frame so the owner can swap schedulers after a reload without remounting. */
  scheduler: DanmakuScheduler | (() => DanmakuScheduler | null);
  requestFrame?: (callback: FrameRequestCallback) => number;
  cancelFrame?: (handle: number) => void;
  now?: () => number;
};

export type DanmakuEngine = {
  /** Feeds a fresh React/anchor sample; rendering continues on `requestAnimationFrame` while playing. */
  sync(state: DanmakuClockState): void;
  destroy(): void;
};

export function interpolateMediaTime(anchor: DanmakuClockAnchor, wallTime: number): number {
  if (!Number.isFinite(anchor.mediaTime)) return 0;
  if (!anchor.playing) return anchor.mediaTime;
  const elapsed = wallTime - anchor.wallTime;
  if (!Number.isFinite(elapsed) || elapsed <= 0) return anchor.mediaTime;
  const rate = Number.isFinite(anchor.playbackRate) && anchor.playbackRate > 0 ? anchor.playbackRate : 1;
  return anchor.mediaTime + (elapsed / 1000) * rate;
}

function roundPosition(value: number) {
  return Math.round(value * 100) / 100;
}

export function createDanmakuEngine({
  container,
  scheduler,
  requestFrame,
  cancelFrame,
  now,
}: DanmakuEngineOptions): DanmakuEngine {
  const nodes = new Map<string, HTMLElement>();
  const request = requestFrame ?? ((callback: FrameRequestCallback) => requestAnimationFrame(callback));
  const cancel = cancelFrame ?? ((handle: number) => cancelAnimationFrame(handle));
  const clock = now
    ?? (() => (typeof performance !== "undefined" ? performance.now() : Date.now()));
  let anchor: DanmakuClockAnchor | null = null;
  let frameHandle: number | null = null;
  let lastScheduler: DanmakuScheduler | null = null;
  let destroyed = false;

  /**
   * Resolves the current scheduler. A replacement (a reload parses new entries, so ids such as
   * `danmaku-0` are reused) drops the existing nodes first: reusing them would keep the previous
   * file's text on screen.
   */
  function resolveScheduler() {
    const active = typeof scheduler === "function" ? scheduler() : scheduler;
    if (active !== lastScheduler) {
      for (const node of nodes.values()) node.remove();
      nodes.clear();
      lastScheduler = active;
    }
    return active;
  }

  function createNode(placement: DanmakuPlacement) {
    const node = container.ownerDocument.createElement("span");
    node.className = `lux-player-danmaku-text lux-player-danmaku-${placement.mode}`;
    node.dataset.danmakuId = placement.id;
    node.style.color = placement.color;
    node.style.fontSize = `${placement.fontSize}px`;
    // Text nodes only: danmaku content is untrusted and is never parsed as markup.
    node.textContent = placement.text;
    return node;
  }

  function render(mediaTime: number) {
    const placements = resolveScheduler()?.place(mediaTime) ?? [];
    const active = new Set<string>();
    for (const placement of placements) {
      active.add(placement.id);
      let node = nodes.get(placement.id);
      if (!node) {
        node = createNode(placement);
        nodes.set(placement.id, node);
        container.append(node);
      }
      node.style.transform = `translate3d(${roundPosition(placement.x)}px, ${roundPosition(placement.y)}px, 0)`;
    }
    for (const [id, node] of nodes) {
      if (active.has(id)) continue;
      node.remove();
      nodes.delete(id);
    }
  }

  function stopLoop() {
    if (frameHandle === null) return;
    cancel(frameHandle);
    frameHandle = null;
  }

  function tick() {
    frameHandle = null;
    if (destroyed || !anchor) return;
    if (!anchor.playing) return;
    render(interpolateMediaTime(anchor, clock()));
    startLoop();
  }

  function startLoop() {
    if (destroyed || frameHandle !== null) return;
    frameHandle = request(tick);
  }

  return {
    sync(state) {
      if (destroyed) return;
      const wallTime = clock();
      const previous = anchor;
      anchor = {
        mediaTime: Number.isFinite(state.mediaTime) ? state.mediaTime : 0,
        wallTime,
        playing: state.playing,
        playbackRate: state.playbackRate,
      };
      const jump = previous
        ? Math.abs(anchor.mediaTime - interpolateMediaTime(previous, wallTime))
        : Number.POSITIVE_INFINITY;
      if (jump > DANMAKU_SEEK_THRESHOLD_SECONDS) resolveScheduler()?.resync();
      // Seek results are drawn at once instead of animating from the stale position.
      render(anchor.mediaTime);
      if (anchor.playing) startLoop();
      else stopLoop();
    },
    destroy() {
      destroyed = true;
      anchor = null;
      stopLoop();
      for (const node of nodes.values()) node.remove();
      nodes.clear();
    },
  };
}
