// @vitest-environment jsdom

import Hls from "hls.js";
import { describe, expect, it, vi } from "vitest";
import { HlsVideoEngine, canUseHls } from "../src/features/player/hls-playback-engine";

describe("HlsVideoEngine", () => {
  it("uses the browser's native HLS path when MSE HLS is unavailable", async () => {
    vi.spyOn(Hls, "isSupported").mockReturnValue(false);
    const video = document.createElement("video");
    vi.spyOn(video, "canPlayType").mockReturnValue("maybe");
    const load = vi.spyOn(video, "load").mockImplementation(() => undefined);
    vi.spyOn(video, "pause").mockImplementation(() => undefined);
    const engine = new HlsVideoEngine(video);

    expect(canUseHls(video)).toBe(true);
    await engine.setSource("/api/v1/playback/sessions/session-1/hls/index.m3u8");

    expect(video.src).toContain("/api/v1/playback/sessions/session-1/hls/index.m3u8");
    expect(load).toHaveBeenCalledOnce();
    engine.destroy();
    expect(video.getAttribute("src")).toBeNull();
  });
});
