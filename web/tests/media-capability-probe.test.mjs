import assert from "node:assert/strict";
import test from "node:test";
import {
  CODEC_PRESETS,
  buildDecoderConfig,
  normalizeNativeSupport,
} from "../public/media-capability-probe.js";

test("normalizes native video capability results", () => {
  assert.equal(normalizeNativeSupport("probably"), "supported");
  assert.equal(normalizeNativeSupport("maybe"), "maybe");
  assert.equal(normalizeNativeSupport(""), "unsupported");
  assert.equal(normalizeNativeSupport(undefined), "unsupported");
});

test("builds a safe WebCodecs configuration from probe input", () => {
  assert.deepEqual(
    buildDecoderConfig({
      codec: "hvc1.2.4.L153.B0",
      width: "3840",
      height: "2160",
      bitrate: "25000000",
      framerate: "30",
    }),
    {
      codec: "hvc1.2.4.L153.B0",
      codedWidth: 3840,
      codedHeight: 2160,
      bitrate: 25_000_000,
      framerate: 30,
      hardwareAcceleration: "prefer-hardware",
    },
  );
});

test("ships probe presets for 4K HEVC and a browser baseline", () => {
  assert.deepEqual(
    CODEC_PRESETS.map(({ id }) => id),
    ["hevc-main-4k", "hevc-main10-hdr10-4k", "avc-4k"],
  );
  assert.equal(CODEC_PRESETS[1].width, 3840);
  assert.equal(CODEC_PRESETS[1].height, 2160);
  assert.equal(CODEC_PRESETS[1].bitDepth, 10);
  assert.equal(CODEC_PRESETS[1].hdr, "HDR10");
});
