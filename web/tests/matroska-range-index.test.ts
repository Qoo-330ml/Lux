import { describe, expect, it } from "vitest";
import { cueForTime, MatroskaIndexError, parseMatroskaRangeIndex } from "../src/features/player/matroska-range-index";

function vint(value: number) {
  if (value < 0x7f) return new Uint8Array([0x80 | value]);
  if (value < 0x3fff) return new Uint8Array([0x40 | (value >> 8), value & 0xff]);
  throw new Error("test value too large");
}
function element(id: number[], payload: Uint8Array) {
  return new Uint8Array([...id, ...vint(payload.byteLength), ...payload]);
}
function uint(value: number, bytes = 1) {
  const output = new Uint8Array(bytes);
  for (let index = bytes - 1; index >= 0; index -= 1) { output[index] = value & 0xff; value >>>= 8; }
  return output;
}
function concat(...parts: Uint8Array[]) { return Uint8Array.from(parts.flatMap((part) => [...part])); }

describe("Matroska SeekHead/Cues index", () => {
  it("resolves Cues through a Segment-relative SeekPosition and selects the prior keyframe", () => {
    const cues = element([0x1c, 0x53, 0xbb, 0x6b], element([0xbb], concat(
      element([0xb3], uint(100)),
      element([0xb7], concat(element([0xf7], uint(1)), element([0xf1], uint(400, 2)), element([0x53, 0x78], uint(1)))),
    )));
    const makeSeekHead = (position: number) => element([0x11, 0x4d, 0x9b, 0x74], element([0x4d, 0xbb], concat(
      element([0x53, 0xab], new Uint8Array([0x1c, 0x53, 0xbb, 0x6b])),
      element([0x53, 0xac], uint(position, 2)),
    )));
    const seekHead = makeSeekHead(0);
    const finalSeekHead = makeSeekHead(seekHead.byteLength);
    const segmentPayload = concat(finalSeekHead, cues);
    const segment = element([0x18, 0x53, 0x80, 0x67], segmentPayload);
    const index = parseMatroskaRangeIndex(concat(new Uint8Array([0x1a, 0x45, 0xdf, 0xa3, 0x80]), segment), 1_000);

    expect(index.segmentDataOffset).toBe(5 + 5);
    expect(index.cues.length).toBe(1);
    expect(index.cues[0]).toMatchObject({ timecode: 100, track: 1, clusterOffset: index.segmentDataOffset + 400, blockNumber: 1 });
    expect(cueForTime(index, 100, 1)?.clusterOffset).toBe(index.segmentDataOffset + 400);
  });

  it("rejects a missing Cues entry and out-of-bounds cue cluster", () => {
    const missing = element([0x18, 0x53, 0x80, 0x67], element([0x11, 0x4d, 0x9b, 0x74], new Uint8Array()));
    expect(() => parseMatroskaRangeIndex(missing)).toThrow(MatroskaIndexError);
    expect(() => parseMatroskaRangeIndex(new Uint8Array([0x18, 0x53, 0x80, 0x67, 0x80]))).toThrow("SeekHead");
  });
});
