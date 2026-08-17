import { describe, expect, it } from "vitest";
import { encodedVideoDurationTicks, isSupportedMatroskaAudio, isSupportedMatroskaVideo, matroskaAudioConfig, toAnnexB } from "../src/features/player/mkv-transcode";

describe("MKV transcode input", () => {
  it("accepts HEVC and AAC-LC Matroska tracks only", () => {
    expect(isSupportedMatroskaVideo({ codecId: "V_MPEGH/ISO/HEVC", codecPrivate: new Uint8Array([1]) })).toBe(true);
    expect(isSupportedMatroskaVideo({ codecId: "V_MPEG4/ISO/AVC", codecPrivate: new Uint8Array([1]) })).toBe(false);
    expect(isSupportedMatroskaAudio({ codecId: "A_AAC/MPEG4/LC", codecPrivate: new Uint8Array([0x12, 0x10]) })).toBe(true);
    expect(isSupportedMatroskaAudio({ codecId: "A_OPUS", codecPrivate: new Uint8Array([1]) })).toBe(false);
    expect(matroskaAudioConfig({ codecId: "A_AAC/MPEG4/LC", codecPrivate: new Uint8Array([0x12, 0x10]), sampleRate: 48_000, channels: 2 })).toEqual({
      codec: "mp4a.40.2",
      asc: new Uint8Array([0x12, 0x10]),
      sampleRate: 48_000,
      channels: 2,
    });
  });

  it("keeps Annex-B samples and converts four-byte length-prefixed NAL units", () => {
    const annexB = new Uint8Array([0, 0, 0, 1, 0x26, 0, 0, 1, 0x02]);
    expect([...toAnnexB(annexB)]).toEqual([...annexB]);
    const lengthPrefixed = new Uint8Array([0, 0, 0, 2, 0x26, 0x01, 0, 0, 0, 1, 0x02]);
    expect([...toAnnexB(lengthPrefixed)]).toEqual([0, 0, 0, 1, 0x26, 0x01, 0, 0, 0, 1, 0x02]);
  });

  it("converts WebCodecs microsecond durations to the 90 kHz MP4 video timescale", () => {
    expect(encodedVideoDurationTicks(40_000, 40)).toBe(3_600);
    expect(encodedVideoDurationTicks(undefined, 40)).toBe(3_600);
  });
});
