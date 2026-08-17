// @vitest-environment jsdom

import { describe, expect, it, vi } from "vitest";
import { hasClientHevcCandidate, hasClientMkvCandidate, shouldUseClientHevc, shouldUseClientMkv } from "../src/features/player/playback-selection";

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
    expect(hasClientHevcCandidate({ ...source, sourceKind: "STRM_URL", externalUrl: "https://media.example.test/video.mp4" })).toBe(true);
    expect(hasClientHevcCandidate({ ...source, streams: [{ index: 0, type: "VIDEO", codec: "dvh1.05.06" }] })).toBe(false);
  });

  it("recognizes MKV HEVC with AAC as a separate client-fallback candidate", async () => {
    const mkv = { ...source, container: "mkv", streams: [
      { index: 0, type: "VIDEO", codec: "HEVC", details: { width: 1920, height: 1080 } },
      { index: 1, type: "AUDIO", codec: "AAC" },
    ] };
    expect(hasClientMkvCandidate(mkv)).toBe(true);
    expect(hasClientMkvCandidate({ ...mkv, streams: [{ index: 0, type: "VIDEO", codec: "HEVC" }, { index: 1, type: "AUDIO", codec: "DTS" }] })).toBe(false);
    vi.stubGlobal("MediaSource", { isTypeSupported: () => true });
    vi.stubGlobal("Worker", class Worker {});
    vi.stubGlobal("VideoEncoder", class VideoEncoder { static isConfigSupported() { return Promise.resolve({ supported: true }); } });
    const video = document.createElement("video");
    vi.spyOn(video, "canPlayType").mockReturnValue("");
    expect(await shouldUseClientMkv(mkv, video)).toBe(true);
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

  it("probes a 4K-capable H.264 level for 4K HEVC sources", async () => {
    let config: Record<string, unknown> | undefined;
    vi.stubGlobal("MediaSource", { isTypeSupported: () => true });
    vi.stubGlobal("Worker", class Worker {});
    vi.stubGlobal("VideoEncoder", class VideoEncoder {
      static isConfigSupported(nextConfig: Record<string, unknown>) {
        config = nextConfig;
        return Promise.resolve({ supported: true });
      }
    });
    const video = document.createElement("video");
    vi.spyOn(video, "canPlayType").mockReturnValue("");

    expect(await shouldUseClientHevc({
      ...source,
      bitrate: 8_000_000,
      streams: [{ index: 0, type: "VIDEO", codec: "HEVC", details: { width: 3840, height: 2160, averageFrameRate: 30 } }],
    }, video)).toBe(true);
    expect(config?.codec).toBe("avc1.640033");
  });
});
