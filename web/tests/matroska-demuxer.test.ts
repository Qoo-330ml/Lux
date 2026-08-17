import { describe, expect, it } from "vitest";
import { MatroskaStreamDemuxer, parseMatroska } from "../src/features/player/matroska-demuxer";

function vint(value: number) {
  if (value < 0x7f) return new Uint8Array([0x80 | value]);
  if (value < 0x3fff) return new Uint8Array([0x40 | (value >> 8), value & 0xff]);
  throw new Error("test value too large");
}

function element(id: number[], payload: Uint8Array) {
  return new Uint8Array([...id, ...vint(payload.byteLength), ...payload]);
}

function uint(value: number, bytes = 1) {
  const result = new Uint8Array(bytes);
  const view = new DataView(result.buffer);
  if (bytes === 1) view.setUint8(0, value);
  else if (bytes === 2) view.setUint16(0, value);
  else if (bytes === 4) view.setUint32(0, value);
  else throw new Error("unsupported test integer width");
  return result;
}

function text(value: string) {
  return new TextEncoder().encode(value);
}

function float(value: number) {
  const result = new Uint8Array(8);
  new DataView(result.buffer).setFloat64(0, value);
  return result;
}

function concat(...parts: Uint8Array[]) {
  return Uint8Array.from(parts.flatMap((part) => [...part]));
}

function simpleBlock(track: number, timecode: number, keyframe: boolean, payload: Uint8Array) {
  const header = new Uint8Array([0x80 | track, (timecode >> 8) & 0xff, timecode & 0xff, keyframe ? 0x80 : 0x00]);
  return element([0xa3], concat(header, payload));
}

describe("Matroska demuxer", () => {
  it("extracts HEVC video and AAC audio samples with cluster timestamps", () => {
    const videoTrack = element([0xae], concat(
      element([0xd7], uint(1)),
      element([0x83], uint(1)),
      element([0x86], text("V_MPEGH/ISO/HEVC")),
      element([0x63, 0xa2], new Uint8Array([1, 2, 3, 4])),
      element([0x23, 0xe3, 0x83], uint(40_000_000, 4)),
      element([0xe0], concat(element([0xb0], uint(1920, 2)), element([0xba], uint(1080, 2)))),
    ));
    const audioTrack = element([0xae], concat(
      element([0xd7], uint(2)),
      element([0x83], uint(2)),
      element([0x86], text("A_AAC")),
      element([0x63, 0xa2], new Uint8Array([0x12, 0x10])),
      element([0xe1], concat(element([0xb5], float(48_000)), element([0x9f], uint(2)))),
    ));
    const cluster = element([0x1f, 0x43, 0xb6, 0x75], concat(
      element([0xe7], uint(1000, 2)),
      simpleBlock(1, 0, true, new Uint8Array([0, 0, 0, 1, 0x26])),
      simpleBlock(2, 0, true, new Uint8Array([0xaa, 0xbb])),
      simpleBlock(1, 40, false, new Uint8Array([0, 0, 0, 1, 0x02])),
    ));
    const source = concat(
      element([0x1a, 0x45, 0xdf, 0xa3], new Uint8Array()),
      element([0x18, 0x53, 0x80, 0x67], concat(
        element([0x15, 0x49, 0xa9, 0x66], element([0x2a, 0xd7, 0xb1], uint(1_000_000, 4))),
        element([0x16, 0x54, 0xae, 0x6b], concat(videoTrack, audioTrack)),
        cluster,
      )),
    );

    const result = parseMatroska(source);

    expect(result.timecodeScale).toBe(1_000_000);
    expect(result.videoTrack).toMatchObject({ number: 1, codecId: "V_MPEGH/ISO/HEVC", width: 1920, height: 1080 });
    expect(result.audioTrack).toMatchObject({ number: 2, codecId: "A_AAC", sampleRate: 48_000, channels: 2 });
    expect(result.videoSamples.map((sample) => ({ timestampMs: sample.timestampMs, durationMs: sample.durationMs, keyframe: sample.keyframe }))).toEqual([
      { timestampMs: 1000, durationMs: 40, keyframe: true },
      { timestampMs: 1040, durationMs: 40, keyframe: false },
    ]);
    expect([...result.videoSamples[0].data]).toEqual([0, 0, 0, 1, 0x26]);
    expect(result.audioSamples.map((sample) => sample.timestampMs)).toEqual([1000]);
    expect([...result.audioSamples[0].data]).toEqual([0xaa, 0xbb]);

    const streamedTracks: string[] = [];
    const streamedSamples: number[] = [];
    const stream = new MatroskaStreamDemuxer({
      onTrack: (track) => streamedTracks.push(`${track.type}:${track.number}`),
      onSample: (sample) => streamedSamples.push(sample.timestampMs),
    });
    for (let offset = 0; offset < source.byteLength; offset += 3) stream.write(source.slice(offset, offset + 3));
    stream.end();

    expect(streamedTracks).toEqual(["video:1", "audio:2"]);
    expect(streamedSamples).toEqual([1000, 1000, 1040]);
  });
});
