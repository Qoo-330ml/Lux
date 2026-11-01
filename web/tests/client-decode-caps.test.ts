import { describe, expect, it } from "vitest";
import {
  DEFAULT_VIDEO_HEIGHT,
  DEFAULT_VIDEO_WIDTH,
  h264CodecForDimensions,
  hevcMseCodecCandidates,
  mseVideoCodecString,
  videoBitDepth,
  videoStreamFacts,
  wasmSoftDecodeExpectsRealtime,
} from "../src/features/player/client-decode-caps";

function videoStream(details: Record<string, unknown>, codec = "h264") {
  return { index: 0, type: "VIDEO", codec, details };
}

describe("client decode capabilities", () => {
  it("reads the capitalized ffprobe detail keys the API actually sends", () => {
    const facts = videoStreamFacts(videoStream({
      Width: 3840,
      Height: 2160,
      BitDepth: 10,
      Profile: "Main 10",
      PixelFormat: "yuv420p10le",
    }, "hevc"));

    expect(facts).toMatchObject({
      codec: "hevc",
      family: "hevc",
      width: 3840,
      height: 2160,
      bitDepth: 10,
      profile: "Main 10",
    });
  });

  it("falls back to the 1080p assumption only when the probe has no dimensions", () => {
    const facts = videoStreamFacts({ index: 0, type: "VIDEO", codec: "h264" });

    expect(facts.width).toBe(DEFAULT_VIDEO_WIDTH);
    expect(facts.height).toBe(DEFAULT_VIDEO_HEIGHT);
    expect(facts.bitDepth).toBe(8);
  });

  it("infers a 10-bit pixel format even without an explicit BitDepth", () => {
    expect(videoBitDepth(videoStream({ PixelFormat: "yuv420p10le" }))).toBe(10);
    expect(videoBitDepth(videoStream({ PixelFormat: "yuv420p10be" }))).toBe(10);
    expect(videoBitDepth(videoStream({ PixelFormat: "p010le" }))).toBe(10);
    expect(videoBitDepth(videoStream({ PixelFormat: "yuv444p12le" }))).toBe(12);
    expect(videoBitDepth(videoStream({ PixelFormat: "gbrp10le" }, "hevc"))).toBe(10);
  });

  it("keeps 8-bit pixel formats at 8 bits", () => {
    expect(videoBitDepth(videoStream({ PixelFormat: "yuv420p" }))).toBe(8);
    expect(videoBitDepth(videoStream({ PixelFormat: "nv12" }))).toBe(8);
    expect(videoBitDepth(videoStream({ PixelFormat: "yuvj420p" }))).toBe(8);
    expect(videoBitDepth(videoStream({}))).toBe(8);
  });

  it("prefers an explicit BitDepth and falls back to the profile label", () => {
    expect(videoBitDepth(videoStream({ BitDepth: "10", PixelFormat: "yuv420p10le" }))).toBe(10);
    expect(videoBitDepth(videoStream({ Profile: "Main 10" }, "hevc"))).toBe(10);
    expect(videoBitDepth(videoStream({ Profile: "High 4:4:4 Predictive" }))).toBe(8);
  });

  it("probes the real bit depth and level instead of a hard-coded 8-bit H.264 string", () => {
    expect(mseVideoCodecString(videoStreamFacts(videoStream({ Width: 1280, Height: 720 })))).toBe("avc1.640028");
    expect(mseVideoCodecString(videoStreamFacts(videoStream({ Width: 1920, Height: 1080 })))).toBe("avc1.64002a");
    expect(mseVideoCodecString(videoStreamFacts(videoStream({
      Width: 1920,
      Height: 1080,
      PixelFormat: "yuv420p10le",
    })))).toBe("avc1.6E002a");
    expect(mseVideoCodecString(videoStreamFacts(videoStream({ Width: 3840, Height: 2160 }, "hevc"))))
      .toBe("hvc1.2.4.L120.B0");
    expect(mseVideoCodecString(videoStreamFacts(videoStream({ Width: 1920, Height: 1080 }, "vp9"))))
      .toBe("vp09.00.10.08");
    expect(mseVideoCodecString(videoStreamFacts(videoStream({
      Width: 1920,
      Height: 1080,
      PixelFormat: "yuv420p10le",
    }, "vp9")))).toBe("vp09.02.10.10");
    expect(mseVideoCodecString(videoStreamFacts(videoStream({ Width: 1280, Height: 720 }, "av1"))))
      .toBe("av01.0.04M.08");
    expect(mseVideoCodecString(videoStreamFacts(videoStream({
      Width: 1920,
      Height: 1080,
      PixelFormat: "yuv420p10le",
    }, "av1")))).toBe("av01.0.04M.10");
    expect(mseVideoCodecString(videoStreamFacts(videoStream({}, "mpeg4")))).toBeNull();
  });

  it("keeps the H.264 level ladder tied to the source dimensions", () => {
    expect(h264CodecForDimensions(1280, 720)).toBe("avc1.640028");
    expect(h264CodecForDimensions(1920, 1080)).toBe("avc1.64002a");
    expect(h264CodecForDimensions(2560, 1440)).toBe("avc1.640033");
    expect(h264CodecForDimensions(3840, 2160)).toBe("avc1.640033");
  });

  it("probes the HEVC level and profile the probe actually reported", () => {
    const facts = videoStreamFacts(videoStream({
      Width: 3840,
      Height: 2160,
      Level: 153,
      Profile: "Main",
      PixelFormat: "yuv420p",
    }, "hevc"));

    expect(facts.level).toBe(153);
    expect(hevcMseCodecCandidates(facts)[0]).toBe("hvc1.1.6.L153.B0");
  });

  it("uses Main 10 for a 10-bit HEVC source without an explicit profile", () => {
    const facts = videoStreamFacts(videoStream({
      Width: 3840,
      Height: 2160,
      Level: 153,
      PixelFormat: "yuv420p10le",
    }, "hevc"));

    expect(hevcMseCodecCandidates(facts)[0]).toBe("hvc1.2.4.L153.B0");
  });

  it("only offers Main 10 HEVC MSE strings to a 10-bit source", () => {
    const tenBit = hevcMseCodecCandidates(videoStreamFacts(videoStream({
      PixelFormat: "yuv420p10le",
    }, "hevc")));
    const eightBit = hevcMseCodecCandidates(videoStreamFacts(videoStream({}, "hevc")));

    expect(tenBit.every((codec) => codec.startsWith("hvc1.2.4."))).toBe(true);
    // An 8-bit source keeps the historical candidate list so Safari/MSE probing does not shrink.
    expect(eightBit).toContain("hvc1.1.6.L120.B0");
    expect(eightBit).toContain("hvc1.2.4.L120.B0");
  });

  it("caps WASM soft decoding at 1080p", () => {
    expect(wasmSoftDecodeExpectsRealtime(videoStreamFacts(videoStream({ Width: 1920, Height: 1080 }, "hevc")))).toBe(true);
    expect(wasmSoftDecodeExpectsRealtime(videoStreamFacts(videoStream({ Width: 1280, Height: 720 }, "hevc")))).toBe(true);
    expect(wasmSoftDecodeExpectsRealtime(videoStreamFacts(videoStream({ Width: 3840, Height: 2160 }, "hevc")))).toBe(false);
    expect(wasmSoftDecodeExpectsRealtime(videoStreamFacts(videoStream({ Width: 2560, Height: 1440 }, "hevc")))).toBe(false);
    // Unknown dimensions fall back to the 1080p assumption instead of disabling the path.
    expect(wasmSoftDecodeExpectsRealtime(videoStreamFacts({ index: 0, type: "VIDEO", codec: "hevc" }))).toBe(true);
  });
});
