import { describe, expect, it, vi } from "vitest";
import {
  initialLuxPlayerState,
  reduceLuxPlayerState,
} from "../src/features/player/core/player-state";
import {
  LuxPlaybackEngineHandle,
  type LuxPlaybackEngine,
} from "../src/features/player/core/playback-engine";
import type {
  LuxPlaybackEngineEvent,
  LuxPlaybackSnapshot,
  LuxPlaybackSource,
} from "../src/features/player/core/types";

const source: LuxPlaybackSource = {
  id: "source-1",
  url: "/signed/source-1",
};

const snapshot: LuxPlaybackSnapshot = {
  currentTime: 12,
  duration: 120,
  bufferedEnd: 30,
  ended: false,
};

describe("LuxPlayer core state", () => {
  it("loads a source and accepts only events for the active generation", () => {
    const preparing = reduceLuxPlayerState(initialLuxPlayerState(), {
      type: "LOAD",
      source,
    });
    expect(preparing.status).toBe("PREPARING");
    expect(preparing.generation).toBe(1);

    const ready = reduceLuxPlayerState(preparing, {
      type: "SOURCE_READY",
      generation: preparing.generation,
      snapshot,
    });
    expect(ready.status).toBe("READY");
    expect(ready.snapshot).toEqual(snapshot);

    const stale = reduceLuxPlayerState(ready, {
      type: "PLAY",
      generation: preparing.generation - 1,
    });
    expect(stale).toBe(ready);
  });

  it("models play, buffering, seek, pause, and end without losing the snapshot", () => {
    const preparing = reduceLuxPlayerState(initialLuxPlayerState(), {
      type: "LOAD",
      source,
    });
    const ready = reduceLuxPlayerState(preparing, {
      type: "SOURCE_READY",
      generation: preparing.generation,
      snapshot,
    });
    const playing = reduceLuxPlayerState(ready, {
      type: "PLAY",
      generation: ready.generation,
    });
    const buffering = reduceLuxPlayerState(playing, {
      type: "WAITING",
      generation: playing.generation,
    });
    const seeking = reduceLuxPlayerState(buffering, {
      type: "SEEK_START",
      generation: buffering.generation,
      position: 80,
    });
    const paused = reduceLuxPlayerState(seeking, {
      type: "SEEKED",
      generation: seeking.generation,
      snapshot: { ...snapshot, currentTime: 80, bufferedEnd: 80 },
      playing: false,
    });
    const ended = reduceLuxPlayerState(paused, {
      type: "ENDED",
      generation: paused.generation,
      snapshot: { ...paused.snapshot, currentTime: 120, ended: true },
    });

    expect(buffering.status).toBe("BUFFERING");
    expect(seeking.status).toBe("SEEKING");
    expect(paused.status).toBe("PAUSED");
    expect(ended.status).toBe("ENDED");
    expect(ended.snapshot.currentTime).toBe(120);
  });

  it("keeps a diagnostic recoverable error and rejects invalid old events", () => {
    const preparing = reduceLuxPlayerState(initialLuxPlayerState(), {
      type: "LOAD",
      source,
    });
    const failed = reduceLuxPlayerState(preparing, {
      type: "ERROR",
      generation: preparing.generation,
      error: {
        code: "ENGINE_FAILED",
        message: "媒体引擎失败",
        recoverable: true,
        canFallback: true,
      },
    });

    expect(failed.status).toBe("FAILED");
    expect(failed.error?.canFallback).toBe(true);
    expect(
      reduceLuxPlayerState(failed, {
        type: "PLAY",
        generation: failed.generation,
      }),
    ).toBe(failed);
  });
});

describe("LuxPlayer engine lifecycle", () => {
  it("makes destroy idempotent and blocks events from a destroyed engine", () => {
    const listeners = new Set<(event: LuxPlaybackEngineEvent) => void>();
    const engine: LuxPlaybackEngine = {
      kind: "native",
      element: {} as HTMLVideoElement,
      performance: null,
      error: null,
      setSource: vi.fn(async () => undefined),
      play: vi.fn(async () => undefined),
      pause: vi.fn(),
      seek: vi.fn(),
      snapshot: vi.fn(() => snapshot),
      subscribe(listener) {
        listeners.add(listener);
        return () => listeners.delete(listener);
      },
      destroy: vi.fn(),
    };
    const handle = new LuxPlaybackEngineHandle(engine);
    const received: LuxPlaybackEngineEvent[] = [];
    handle.subscribe((event) => received.push(event));

    listeners.forEach((listener) => listener({ type: "PLAYING" }));
    handle.destroy();
    handle.destroy();
    listeners.forEach((listener) => listener({ type: "ENDED", snapshot }));

    expect(received).toEqual([{ type: "PLAYING" }]);
    expect(engine.destroy).toHaveBeenCalledTimes(1);
    expect(handle.isDestroyed).toBe(true);
  });
});
