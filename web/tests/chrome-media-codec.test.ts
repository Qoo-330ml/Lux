import { describe, expect, it } from "vitest";
import {
  canonicalAudioCodec,
  canonicalVideoCodec,
  classifyChromeExtensionMedia,
} from "../src/features/player/chrome-media-codec";

describe("Chrome extension media codec selection", () => {
  it("normalizes the four supported video families", () => {
    expect(canonicalVideoCodec("H.264")).toBe("avc1");
    expect(canonicalVideoCodec("V_MPEGH/ISO/HEVC")).toBe("hevc");
    expect(canonicalVideoCodec("hvc1.2.4.L153.B0")).toBe("hevc");
    expect(canonicalVideoCodec("vp09.00.10.08")).toBe("vp09");
    expect(canonicalVideoCodec("av01.0.04M.08")).toBe("av01");
  });

  it("normalizes AAC, Opus, AC-3 and E-AC-3", () => {
    expect(canonicalAudioCodec("A_AAC")).toBe("mp4a.40.2");
    expect(canonicalAudioCodec("opus")).toBe("opus");
    expect(canonicalAudioCodec("A_AC3")).toBe("ac-3");
    expect(canonicalAudioCodec("ec-3")).toBe("ec-3");
  });

  it("keeps an already supported native combination on the native path", () => {
    expect(classifyChromeExtensionMedia({
      videoCodec: "h264",
      audioCodec: "aac",
      nativeMseSupported: true,
    })).toMatchObject({
      video: "native-mse",
      audio: "native-mse",
      requiresExtension: false,
      supported: true,
    });
  });

  it("selects the existing HEVC WASM output path when native MSE is unavailable", () => {
    expect(classifyChromeExtensionMedia({
      videoCodec: "hevc",
      audioCodec: "opus",
      nativeMseSupported: false,
      hevcWasmAvailable: true,
      h264OutputSupported: true,
    })).toMatchObject({
      video: "hevc-wasm",
      audio: "native-mse",
      requiresExtension: true,
      supported: true,
    });
  });

  it("does not claim E-AC-3 support before the audio decoder is available", () => {
    const capability = classifyChromeExtensionMedia({
      videoCodec: "hevc",
      audioCodec: "eac3",
      nativeMseSupported: false,
      hevcWasmAvailable: true,
      h264OutputSupported: true,
    });
    expect(capability.supported).toBe(false);
    expect(capability.reason).toContain("窄解码器");
  });

  it("allows E-AC-3 only when the explicit WASM path is ready", () => {
    expect(classifyChromeExtensionMedia({
      videoCodec: "av1",
      audioCodec: "ec-3",
      nativeMseSupported: false,
      eac3WasmAvailable: true,
    })).toMatchObject({
      video: "native-mse",
      audio: "eac3-wasm",
      requiresExtension: true,
      supported: true,
    });
  });

  it("rejects DTS and unknown video instead of silently dropping a track", () => {
    expect(classifyChromeExtensionMedia({
      videoCodec: "hevc",
      audioCodec: "dts",
      nativeMseSupported: false,
      hevcWasmAvailable: true,
      h264OutputSupported: true,
    }).supported).toBe(false);
    expect(classifyChromeExtensionMedia({
      videoCodec: "mpeg2video",
      audioCodec: "aac",
      nativeMseSupported: false,
    }).supported).toBe(false);
  });
});
