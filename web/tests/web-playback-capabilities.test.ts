// @vitest-environment jsdom

import { beforeEach, describe, expect, it, vi } from "vitest";
import { webPlaybackCapabilities } from "../src/features/player/playback-capabilities";

/** A browser that *claims* every codec the caller asks about, like Edge does for AC-3. */
function lyingBrowser() {
  const video = document.createElement("video");
  vi.spyOn(video, "canPlayType").mockReturnValue("probably");
  return video;
}

const sourceWith = (videoCodec: string, audioCodec: string | null) => ({
  id: "source-capabilities",
  streams: [
    { index: 0, type: "VIDEO", codec: videoCodec },
    ...(audioCodec ? [{ index: 1, type: "AUDIO", codec: audioCodec }] : []),
  ],
});

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

  it("keeps trusting the browser for the first attempt", () => {
    const capabilities = webPlaybackCapabilities(sourceWith("h264", "ac3"), 0, lyingBrowser());

    expect(capabilities.directPlay).toBe(true);
    expect(capabilities.videoCopyToFmp4).toBe(true);
    expect(capabilities.audioCopyToFmp4).toBe(true);
  });

  it("stops trusting a claimed AC-3/E-AC-3 decode on the retry", () => {
    // The Edge box without the Dolby extension answered yes and then decoded no audio frame.
    for (const codec of ["ac3", "AC3", "ac-3", "eac3", "EAC3", "ec-3"]) {
      const capabilities = webPlaybackCapabilities(sourceWith("h264", codec), 1, lyingBrowser());
      expect(capabilities.audioCopyToFmp4, codec).toBe(false);
      // The video track is still H.264, so the server can keep the audio-only transcode tier.
      expect(capabilities.videoCopyToFmp4, codec).toBe(true);
    }
  });

  it("stops trusting a claimed MPEG-4 Part 2 / VC-1 video decode on the retry", () => {
    for (const codec of ["mpeg4", "mp4v.20.9", "msmpeg4v3", "vc1", "wmv3"]) {
      const capabilities = webPlaybackCapabilities(sourceWith(codec, "aac"), 1, lyingBrowser());
      expect(capabilities.videoCopyToFmp4, codec).toBe(false);
      expect(capabilities.audioCopyToFmp4, codec).toBe(true);
    }
  });

  it("keeps the normal H.264 plus AAC remux on the retry", () => {
    const capabilities = webPlaybackCapabilities(sourceWith("h264", "aac"), 1, lyingBrowser());

    expect(capabilities.directPlay).toBe(false);
    expect(capabilities.videoCopyToFmp4).toBe(true);
    expect(capabilities.audioCopyToFmp4).toBe(true);
  });

  it("forces the audio track through transcoding for a silent native track", () => {
    const capabilities = webPlaybackCapabilities(
      sourceWith("h264", "aac"),
      1,
      lyingBrowser(),
      { forceAudioTranscode: true },
    );

    expect(capabilities.audioCopyToFmp4).toBe(false);
    expect(capabilities.videoCopyToFmp4).toBe(true);
  });

  it("only narrows the claims of a retry, never of the first attempt", () => {
    // The flag exists for the fallback request; a first attempt must keep describing the browser.
    const capabilities = webPlaybackCapabilities(
      sourceWith("h264", "aac"),
      0,
      lyingBrowser(),
      { forceAudioTranscode: true },
    );

    expect(capabilities.directPlay).toBe(true);
    expect(capabilities.audioCopyToFmp4).toBe(true);
  });
});
