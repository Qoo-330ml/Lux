// @vitest-environment jsdom

import { describe, expect, it, vi } from "vitest";
import { NativeVideoEngine, summarizePlaybackPerformance } from "../src/features/player/playback-engine";

describe("NativeVideoEngine", () => {
  it("summarizes client fallback throughput against media time", () => {
    expect(summarizePlaybackPerformance(8_000, 16_000)).toEqual({
      mediaDurationMs: 8_000,
      processingDurationMs: 16_000,
      speedX: 0.5,
      realtime: false,
    });
    expect(summarizePlaybackPerformance(8_000, 4_000)?.realtime).toBe(true);
    expect(summarizePlaybackPerformance(0, 4_000)).toBeNull();
  });

  it("loads a source and exposes the native video snapshot", () => {
    const video = document.createElement("video");
    const load = vi.spyOn(video, "load").mockImplementation(() => undefined);
    vi.spyOn(video, "pause").mockImplementation(() => undefined);
    Object.defineProperty(video, "currentTime", { configurable: true, value: 12 });
    Object.defineProperty(video, "duration", { configurable: true, value: 120 });

    const engine = new NativeVideoEngine(video);
    engine.setSource("/api/v1/items/movie-1/stream?sourceId=source-1", "/poster.jpg");

    expect(engine.kind).toBe("native");
    expect(video.src).toContain("/api/v1/items/movie-1/stream?sourceId=source-1");
    expect(video.poster).toContain("/poster.jpg");
    expect(engine.snapshot()).toEqual({ currentTime: 12, duration: 120, ended: false });
    expect(load).toHaveBeenCalledOnce();
  });

  it("clears the native element when destroyed", () => {
    const video = document.createElement("video");
    const load = vi.spyOn(video, "load").mockImplementation(() => undefined);
    vi.spyOn(video, "pause").mockImplementation(() => undefined);
    const engine = new NativeVideoEngine(video);
    engine.setSource("/stream.mp4");

    engine.destroy();

    expect(video.getAttribute("src")).toBeNull();
    expect(video.getAttribute("poster")).toBeNull();
    expect(load).toHaveBeenCalledTimes(2);
  });
});
