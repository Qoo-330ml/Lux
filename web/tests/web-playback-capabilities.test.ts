// @vitest-environment jsdom

import { beforeEach, describe, expect, it, vi } from "vitest";
import { combinePlayerNotices, remoteAudioCodecWarning, webPlaybackCapabilities } from "../src/features/player/PlayerPage";

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

  it("explains why Chrome has no sound for a remote E-AC-3 STRM", () => {
    expect(remoteAudioCodecWarning({
      id: "remote-eac3",
      sourceKind: "STRM_URL",
      streams: [{ index: 1, type: "AUDIO", codec: "EAC3" }],
    })).toContain("无法解码 E-AC-3 音频，因此画面可能播放但没有声音");
  });

  it("does not warn for a remote AAC STRM", () => {
    expect(remoteAudioCodecWarning({
      id: "remote-aac",
      sourceKind: "STRM_URL",
      streams: [{ index: 1, type: "AUDIO", codec: "AAC" }],
    })).toBeNull();
  });

  it("does not warn when a remote source has a compatible secondary audio track", () => {
    expect(remoteAudioCodecWarning({
      id: "remote-mixed-audio",
      sourceKind: "STRM_URL",
      streams: [
        { index: 1, type: "AUDIO", codec: "EAC3" },
        { index: 2, type: "AUDIO", codec: "AAC" },
      ],
    })).toBeNull();
  });

  it("keeps the caption error and audio warning visible together", () => {
    expect(combinePlayerNotices("远程字幕不可用", "当前 Chrome 无法解码 E-AC-3 音频")).toBe(
      "远程字幕不可用；当前 Chrome 无法解码 E-AC-3 音频",
    );
    expect(combinePlayerNotices(null, undefined)).toBeNull();
  });
});
