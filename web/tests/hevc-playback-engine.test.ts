import { describe, expect, it } from "vitest";
import { copySegmentBytes, normalizeFragmentCompositionOffsets, segmentSampleCount, segmentSampleCountForTracks } from "../src/features/player/hevc-playback-engine";
import { isHevcCodec } from "../src/features/player/media-codec";

describe("HEVC playback engine helpers", () => {
  it("recognizes HEVC codec labels without confusing Dolby Vision metadata", () => {
    expect(isHevcCodec("hvc1.2.4.L153.B0")).toBe(true);
    expect(isHevcCodec("hev1.1.6.L120.B0")).toBe(true);
    expect(isHevcCodec("HEVC")).toBe(true);
    expect(isHevcCodec("dvh1.05.06")).toBe(false);
    expect(isHevcCodec("avc1.640028")).toBe(false);
  });

  it("chooses a bounded segment sample count from track timing", () => {
    expect(segmentSampleCount({ nb_samples: 288, duration: 288, timescale: 24 })).toBe(48);
    expect(segmentSampleCount({ nb_samples: 565, duration: 578560, timescale: 48000 })).toBe(94);
    expect(segmentSampleCount({ nb_samples: 0, duration: 0, timescale: 0 })).toBe(1);
  });

  it("copies initialization bytes before a worker can transfer the source buffer", () => {
    const original = new Uint8Array([1, 2, 3]);
    const copy = copySegmentBytes(original.buffer);

    original[0] = 9;

    expect([...copy]).toEqual([1, 2, 3]);
    expect(copy.buffer).not.toBe(original.buffer);
  });

  it("uses one MP4Box segment sample count for every configured track", () => {
    expect(segmentSampleCountForTracks({ nb_samples: 288, duration: 288, timescale: 24 })).toEqual({ video: 48, audio: 48 });
  });

  it("marks unsigned negative composition offsets as signed trun values", () => {
    const fragment = new Uint8Array(36);
    const view = new DataView(fragment.buffer);
    view.setUint32(0, 36);
    fragment.set([109, 111, 111, 102], 4);
    view.setUint32(8, 28);
    fragment.set([116, 114, 97, 102], 12);
    view.setUint32(16, 20);
    fragment.set([116, 114, 117, 110], 20);
    view.setUint32(24, 0x00000800);
    view.setUint32(28, 1);
    view.setUint32(32, 0xffffffff);

    const normalized = normalizeFragmentCompositionOffsets(fragment.buffer);

    expect(normalized[24]).toBe(1);
    expect(new DataView(normalized.buffer).getUint32(32)).toBe(0xffffffff);
    expect(normalized.buffer).not.toBe(fragment.buffer);
  });
});
