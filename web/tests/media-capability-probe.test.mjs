import assert from "node:assert/strict";
import test from "node:test";
import {
  CODEC_PRESETS,
  buildDecoderConfig,
  normalizeNativeSupport,
  playbackQualityDelta,
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

test("reports playback quality for the current measurement interval", () => {
  assert.deepEqual(
    playbackQualityDelta(
      { droppedVideoFrames: 10, totalVideoFrames: 100 },
      { droppedVideoFrames: 13, totalVideoFrames: 145 },
    ),
    { droppedFrames: 3, totalFrames: 45 },
  );
  assert.deepEqual(playbackQualityDelta(undefined, undefined), { droppedFrames: null, totalFrames: null });
});

test("keeps the standalone probe page free of a default favicon request", async () => {
  const { readFile } = await import("node:fs/promises");
  const html = await readFile(new URL("../public/media-capability-probe.html", import.meta.url), "utf8");
  assert.match(html, /<link rel="icon" href="\/favicon\.svg" \/>/);
});
