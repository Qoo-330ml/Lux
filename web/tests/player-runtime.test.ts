// @vitest-environment jsdom

import { describe, expect, it, vi } from "vitest";
import type { PlaybackEngine } from "../src/features/player/playback-engine";
import { LegacyPlaybackEngineAdapter } from "../src/features/player/core/legacy-engine-adapter";
import { LuxPlayerRuntime } from "../src/features/player/core/player-runtime";
import type {
  LuxPlaybackEngine,
  LuxPlaybackEngineEvent,
} from "../src/features/player/core/playback-engine";
import type {
  LuxPlaybackSnapshot,
  LuxPlaybackSource,
} from "../src/features/player/core/types";

const source: LuxPlaybackSource = {
  id: "source-1",
  url: "/signed/source-1",
};

const snapshot: LuxPlaybackSnapshot = {
  currentTime: 4,
  duration: 100,
  bufferedEnd: 20,
  ended: false,
};

function fakeEngine() {
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
  return {
    engine,
    emit(event: LuxPlaybackEngineEvent) {
      listeners.forEach((listener) => listener(event));
    },
  };
}

describe("LuxPlayer runtime", () => {
  it("loads an engine through the Lux controller and exposes its state", async () => {
    const runtime = new LuxPlayerRuntime();
    const fake = fakeEngine();
    const states: string[] = [];
    runtime.subscribe((state) => states.push(state.status));

    await runtime.load(fake.engine, source);
    fake.emit({ type: "SOURCE_READY", snapshot });
    fake.emit({ type: "PLAYING" });

    expect(fake.engine.setSource).toHaveBeenCalledWith(source);
    expect(runtime.state.status).toBe("PLAYING");
    expect(runtime.state.source).toEqual(source);
    expect(states).toEqual(["PREPARING", "READY", "PLAYING"]);
  });

  it("destroys the previous engine before loading a replacement", async () => {
    const runtime = new LuxPlayerRuntime();
    const first = fakeEngine();
    const second = fakeEngine();

    await runtime.load(first.engine, source);
    await runtime.load(second.engine, { ...source, id: "source-2" });
    first.emit({ type: "PLAYING" });

    expect(first.engine.destroy).toHaveBeenCalledTimes(1);
    expect(runtime.state.status).toBe("PREPARING");
    expect(runtime.state.source?.id).toBe("source-2");
  });

  it("does not apply a late old-engine error to a ready replacement", async () => {
    const runtime = new LuxPlayerRuntime();
    const first = fakeEngine();
    const second = fakeEngine();

    await runtime.load(first.engine, source);
    first.emit({ type: "SOURCE_READY", snapshot });
    await runtime.load(second.engine, { ...source, id: "source-2" });
    second.emit({ type: "SOURCE_READY", snapshot });
    first.emit({
      type: "ERROR",
      error: {
        code: "ENGINE_FAILED",
        message: "old engine failed",
        recoverable: true,
        canFallback: true,
      },
    });

    expect(runtime.state.status).toBe("READY");
    expect(runtime.state.source?.id).toBe("source-2");
    expect(runtime.state.error).toBeNull();
  });

  it("destroys the active engine and resets the controller once", async () => {
    const runtime = new LuxPlayerRuntime();
    const fake = fakeEngine();

    await runtime.load(fake.engine, source);
    runtime.destroy();
    runtime.destroy();

    expect(fake.engine.destroy).toHaveBeenCalledTimes(1);
    expect(runtime.state.status).toBe("IDLE");
    expect(runtime.state.source).toBeNull();
  });

  it("does not publish a second reset when teardown is repeated", async () => {
    const runtime = new LuxPlayerRuntime();
    const fake = fakeEngine();
    const states: string[] = [];
    runtime.subscribe((state) => states.push(`${state.status}:${state.generation}`));

    await runtime.load(fake.engine, source);
    runtime.destroy();
    runtime.destroy();

    expect(states).toEqual(["PREPARING:1", "IDLE:2"]);
  });

  it("destroys a load that finishes after the runtime has been torn down", async () => {
    let resolveSource: (() => void) | undefined;
    const runtime = new LuxPlayerRuntime();
    const fake = fakeEngine();
    fake.engine.setSource = vi.fn(() => new Promise<void>((resolve) => {
      resolveSource = resolve;
    }));

    const load = runtime.load(fake.engine, source);
    runtime.destroy();
    resolveSource?.();
    await load;

    expect(fake.engine.destroy).toHaveBeenCalledTimes(1);
    expect(runtime.state.status).toBe("IDLE");
  });

  it("publishes a loading failure to runtime event listeners", async () => {
    const runtime = new LuxPlayerRuntime();
    const fake = fakeEngine();
    const events: LuxPlaybackEngineEvent[] = [];
    fake.engine.setSource = vi.fn(async () => {
      throw new Error("source rejected");
    });
    runtime.subscribeEvents((event) => events.push(event));

    await expect(runtime.load(fake.engine, source)).rejects.toThrow("source rejected");

    expect(events).toEqual([{
      type: "ERROR",
      error: {
        code: "ENGINE_FAILED",
        message: "source rejected",
        recoverable: true,
        canFallback: true,
      },
    }]);
  });
});

describe("LegacyPlaybackEngineAdapter", () => {
  it("reads dynamic media values when the browser event fires", () => {
    const video = document.createElement("video");
    Object.defineProperty(video, "currentTime", { configurable: true, writable: true, value: 0 });
    Object.defineProperty(video, "paused", { configurable: true, value: true });
    const engine: PlaybackEngine = {
      kind: "native",
      element: video,
      performance: null,
      error: null,
      setSource: vi.fn(async () => undefined),
      play: vi.fn(async () => undefined),
      pause: vi.fn(),
      seek: vi.fn(),
      snapshot: vi.fn(() => ({ currentTime: video.currentTime, duration: 100, ended: false })),
      destroy: vi.fn(),
    };
    const adapter = new LegacyPlaybackEngineAdapter(engine);
    const events: string[] = [];
    const unsubscribe = adapter.subscribe((event) => {
      if (event.type === "SEEK_START") events.push(`${event.type}:${event.position}`);
      if (event.type === "CAN_PLAY") events.push(`${event.type}:${event.playing}`);
    });

    video.currentTime = 42;
    video.dispatchEvent(new Event("seeking"));
    Object.defineProperty(video, "paused", { configurable: true, value: false });
    video.dispatchEvent(new Event("canplay"));
    unsubscribe();

    expect(events).toEqual(["SEEK_START:42", "CAN_PLAY:true"]);
  });

  it("removes DOM listeners when the adapter is destroyed directly", () => {
    const video = document.createElement("video");
    const engine: PlaybackEngine = {
      kind: "native",
      element: video,
      performance: null,
      error: null,
      setSource: vi.fn(async () => undefined),
      play: vi.fn(async () => undefined),
      pause: vi.fn(),
      seek: vi.fn(),
      snapshot: vi.fn(() => ({ currentTime: 0, duration: 100, ended: false })),
      destroy: vi.fn(),
    };
    const adapter = new LegacyPlaybackEngineAdapter(engine);
    const events: LuxPlaybackEngineEvent[] = [];
    adapter.subscribe((event) => events.push(event));

    adapter.destroy();
    video.dispatchEvent(new Event("play"));

    expect(events).toEqual([]);
    expect(engine.destroy).toHaveBeenCalledTimes(1);
  });
});
