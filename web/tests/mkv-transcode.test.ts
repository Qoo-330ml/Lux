import { describe, expect, it } from "vitest";
import { createFile } from "mp4box";
import { addHevcTrack, hevcCodecString, makeAacEsdsData, matroskaTimestampTicks, toLengthPrefixed } from "../src/features/player/mkv-remux";
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

  it("describes Main10 HEVC and scales Matroska timestamps for fMP4", () => {
    const hvcC = new Uint8Array(23);
    hvcC[0] = 1;
    hvcC[1] = 2;
    hvcC[12] = 153;
    expect(hevcCodecString(hvcC)).toBe("hvc1.2.4.L153.B0");
    expect(matroskaTimestampTicks(1_234.5, 90_000)).toBe(111_105);
  });

  it("builds an MPEG-4 AudioSpecificConfig descriptor for AAC fMP4", () => {
    const esds = makeAacEsdsData(new Uint8Array([0x12, 0x10]));
    expect([...esds.slice(0, 4)]).toEqual([0, 0, 0, 0]);
    expect([...esds]).toEqual(expect.arrayContaining([0x12, 0x10]));
  });

  it("converts Annex-B HEVC samples back to the hvcC length-prefixed form", () => {
    expect([...toLengthPrefixed(new Uint8Array([0, 0, 0, 1, 0x40, 0x01, 0, 0, 1, 0x02]))])
      .toEqual([0, 0, 0, 2, 0x40, 0x01, 0, 0, 0, 1, 0x02]);
  });

  it("keeps the Matroska hvcC bytes verbatim in the HEVC sample entry", () => {
    const codecPrivate = new Uint8Array(23);
    codecPrivate[0] = 1;
    codecPrivate[1] = 2;
    codecPrivate[12] = 153;
    const file = createFile();

    const trackId = addHevcTrack(file, codecPrivate, 3840, 2160);
    const track = file.moov.traks.find((entry) => entry.tkhd.track_id === trackId);
    const sampleEntry = track?.mdia.minf.stbl.stsd.entries[0];
    const hvcC = sampleEntry?.boxes?.find((box) => box.type === "hvcC");

    expect(hvcC?.data).toEqual(codecPrivate);
  });
});
