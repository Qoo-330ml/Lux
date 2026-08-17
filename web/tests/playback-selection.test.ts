// @vitest-environment jsdom

import { describe, expect, it, vi } from "vitest";
import { hasClientHevcCandidate, shouldUseClientHevc } from "../src/features/player/playback-selection";

describe("playback selection", () => {
  const source = {
    id: "source-1",
    container: "mp4",
    sourceKind: "LOCAL_FILE",
    streams: [{ index: 0, type: "VIDEO", codec: "HEVC" }],
  };

  it("recognizes local MP4 HEVC sources as client-fallback candidates", () => {
    expect(hasClientHevcCandidate(source)).toBe(true);
    expect(hasClientHevcCandidate({ ...source, container: "mkv" })).toBe(false);
    expect(hasClientHevcCandidate({ ...source, sourceKind: "STRM_URL" })).toBe(false);
    expect(hasClientHevcCandidate({ ...source, streams: [{ index: 0, type: "VIDEO", codec: "dvh1.05.06" }] })).toBe(false);
  });

  it("uses the fallback only when native HEVC is unavailable", async () => {
    vi.stubGlobal("MediaSource", { isTypeSupported: () => true });
    vi.stubGlobal("Worker", class Worker {});
    vi.stubGlobal("VideoEncoder", class VideoEncoder { static isConfigSupported() { return Promise.resolve({ supported: true }); } });
    const video = document.createElement("video");
    vi.spyOn(video, "canPlayType").mockReturnValue("");
    expect(await shouldUseClientHevc(source, video)).toBe(true);

    vi.spyOn(video, "canPlayType").mockReturnValue("probably");
    expect(await shouldUseClientHevc(source, video)).toBe(false);
  });

  it("does not select fallback when the browser lacks a required runtime API", async () => {
    vi.stubGlobal("MediaSource", { isTypeSupported: () => true });
    vi.stubGlobal("Worker", class Worker {});
    vi.stubGlobal("VideoEncoder", undefined);
    const video = document.createElement("video");
    vi.spyOn(video, "canPlayType").mockReturnValue("");

    expect(await shouldUseClientHevc(source, video)).toBe(false);
  });

  it("does not select fallback when H.264 VideoEncoder rejects the source configuration", async () => {
    vi.stubGlobal("MediaSource", { isTypeSupported: () => true });
    vi.stubGlobal("Worker", class Worker {});
    vi.stubGlobal("VideoEncoder", class VideoEncoder { static isConfigSupported() { return Promise.resolve({ supported: false }); } });
    const video = document.createElement("video");
    vi.spyOn(video, "canPlayType").mockReturnValue("");

    expect(await shouldUseClientHevc(source, video)).toBe(false);
  });
});
