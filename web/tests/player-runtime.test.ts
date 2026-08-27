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
});
