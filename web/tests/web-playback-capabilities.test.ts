// @vitest-environment jsdom

import { beforeEach, describe, expect, it, vi } from "vitest";
import { webPlaybackCapabilities } from "../src/features/player/PlayerPage";

describe("webPlaybackCapabilities", () => {
  beforeEach(() => {
    vi.stubGlobal("MediaSource", {
      isTypeSupported: vi.fn(() => false),
    });
  });

  it("does not advertise video copy when the browser cannot consume the codec", () => {
    const video = document.createElement("video");
    vi.spyOn(video, "canPlayType").mockImplementation((mime) =>
      mime === "application/vnd.apple.mpegurl" ? "maybe" : "",
    );

    const capabilities = webPlaybackCapabilities(
      {
        id: "source-1",
        streams: [{ index: 0, type: "VIDEO", codec: "hevc" }],
      },
      1,
      video,
    );

    expect(capabilities.hls).toBe(true);
    expect(capabilities.videoCopyToFmp4).toBe(false);
    expect(capabilities.softwareTranscode).toBe(true);
  });
});
