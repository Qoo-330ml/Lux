import type { MatroskaTrack } from "./matroska-demuxer";

export type MatroskaAudioConfig =
  | { codec: "mp4a.40.2"; asc: Uint8Array; sampleRate: number; channels: number }
  | { codec: "ac-3"; dac3: Uint8Array; sampleRate: number; channels: number }
  | { codec: "ec-3"; dec3: Uint8Array; sampleRate: number; channels: number; frameDurationMs: number };

export function isSupportedMatroskaVideo(track: Pick<MatroskaTrack, "codecId" | "codecPrivate">) {
  return track.codecId.toUpperCase() === "V_MPEGH/ISO/HEVC" && track.codecPrivate.byteLength > 0;
}

export function isSupportedMatroskaAudio(track: Pick<MatroskaTrack, "codecId" | "codecPrivate">) {
  const codec = track.codecId.toUpperCase();
  const audioObjectType = (track.codecPrivate[0] ?? 0) >> 3;
  return (codec.startsWith("A_AAC") && audioObjectType === 2 && track.codecPrivate.byteLength >= 2)
    || (codec === "A_AC3" && (track.codecPrivate.byteLength === 0 || parseAc3Config(track.codecPrivate) !== null))
    || (codec === "A_EAC3" && (track.codecPrivate.byteLength === 0 || parseEac3Config(track.codecPrivate) !== null));
}

export function matroskaAudioConfig(track: Pick<MatroskaTrack, "codecId" | "codecPrivate" | "sampleRate" | "channels">): MatroskaAudioConfig | null {
  if (!track.sampleRate || !track.channels) return null;
  const codec = track.codecId.toUpperCase();
  if (codec.startsWith("A_AAC") && isSupportedMatroskaAudio(track)) {
    return { codec: "mp4a.40.2", asc: track.codecPrivate.slice(), sampleRate: track.sampleRate, channels: track.channels };
  }
  const dac3 = codec === "A_AC3" ? parseAc3Config(track.codecPrivate) : null;
  if (dac3) return { codec: "ac-3", dac3, sampleRate: track.sampleRate, channels: track.channels };
  const eac3 = codec === "A_EAC3" ? parseEac3Config(track.codecPrivate) : null;
  return eac3 ? { codec: "ec-3", dec3: eac3.dec3, sampleRate: track.sampleRate, channels: track.channels, frameDurationMs: eac3.frameDurationMs } : null;
}

export function encodedVideoDurationTicks(durationUs: number | undefined, fallbackDurationMs: number) {
  const durationMs = durationUs !== undefined && Number.isFinite(durationUs) ? durationUs / 1000 : fallbackDurationMs;
  return Math.max(1, Math.round(durationMs * 90));
}

export function toAnnexB(data: Uint8Array) {
  if (startsWithStartCode(data)) return data;
  const output: number[] = [];
  let offset = 0;
  while (offset + 4 <= data.byteLength) {
    const size = new DataView(data.buffer, data.byteOffset + offset, 4).getUint32(0);
    offset += 4;
    if (size <= 0 || offset + size > data.byteLength) return data;
    output.push(0, 0, 0, 1, ...data.subarray(offset, offset + size));
    offset += size;
  }
  return offset === data.byteLength ? Uint8Array.from(output) : data;
}

function startsWithStartCode(data: Uint8Array) {
  return data.byteLength >= 4 && data[0] === 0 && data[1] === 0 && (data[2] === 1 || data[2] === 0 && data[3] === 1);
}

function parseAc3Config(data: Uint8Array) {
  if (data.byteLength < 7 || data[0] !== 0x0b || data[1] !== 0x77) return null;
  const fscod = data[4] >> 6;
  const frameSizeCode = data[4] & 0x3f;
  const bsid = data[5] >> 3;
  if (fscod === 3 || frameSizeCode >= 38 || bsid > 10) return null;
  const bsmod = data[5] & 7;
  let bitOffset = 48;
  const acmod = readBits(data, bitOffset, 3);
  if (acmod === null) return null;
  bitOffset += 3;
  if (acmod === 0) bitOffset += 5;
  else {
    if (acmod & 1) bitOffset += 2;
    if (acmod & 4) bitOffset += 2;
    if (acmod === 2) bitOffset += 2;
  }
  const lfeon = readBits(data, bitOffset, 1);
  if (lfeon === null) return null;
  const dac3 = new Uint8Array(3);
  dac3[0] = (fscod << 6) | (bsid << 1) | (bsmod >> 2);
  dac3[1] = ((bsmod & 3) << 6) | (acmod << 3) | (lfeon << 2) | ((frameSizeCode >> 1) & 3);
  dac3[2] = ((frameSizeCode >> 2) & 7) << 5;
  return dac3;
}

function parseEac3Config(data: Uint8Array) {
  if (data.byteLength < 6 || data[0] !== 0x0b || data[1] !== 0x77) return null;
  let bitOffset = 16;
  const strmtyp = readBits(data, bitOffset, 2);
  bitOffset += 2;
  const substreamId = readBits(data, bitOffset, 3);
  bitOffset += 3;
  const frameSize = readBits(data, bitOffset, 11);
  bitOffset += 11;
  const fscod = readBits(data, bitOffset, 2);
  bitOffset += 2;
  if (strmtyp === null || substreamId === null || frameSize === null || fscod === null) return null;
  let numBlocksCode: number | null;
  let sampleRateCode: number | null = fscod;
  if (fscod === 3) {
    sampleRateCode = readBits(data, bitOffset, 2);
    bitOffset += 2;
    numBlocksCode = readBits(data, bitOffset, 2);
    bitOffset += 2;
  } else {
    numBlocksCode = readBits(data, bitOffset, 2);
    bitOffset += 2;
  }
  const acmod = readBits(data, bitOffset, 3);
  bitOffset += 3;
  const lfeon = readBits(data, bitOffset, 1);
  bitOffset += 1;
  const bsid = readBits(data, bitOffset, 5);
  if (sampleRateCode === null || numBlocksCode === null || acmod === null || lfeon === null || bsid === null) return null;
  if (strmtyp !== 0 || substreamId !== 0 || sampleRateCode > 2 || numBlocksCode > 3 || bsid < 11 || bsid > 16) return null;
  const blocks = [1, 2, 3, 6][numBlocksCode];
  const sampleRate = [48_000, 44_100, 32_000][sampleRateCode];
  if (!blocks || !sampleRate) return null;
  // EC3SpecificBox: data_rate/num_ind_sub followed by one four-byte
  // independent substream descriptor. E-AC-3's syncframe has no bsmod field,
  // so it is 0; the final byte is the required reserved bit plus padding.
  const dec3 = new Uint8Array([
    0,
    0,
    (sampleRateCode << 6) | (bsid << 1),
    (acmod << 1) | lfeon,
    0,
    0,
  ]);
  return { dec3, frameDurationMs: blocks * 256_000 / sampleRate };
}

function readBits(data: Uint8Array, offset: number, length: number) {
  if (offset + length > data.byteLength * 8) return null;
  let value = 0;
  for (let index = 0; index < length; index += 1) {
    value = (value << 1) | ((data[(offset + index) >> 3] >> (7 - ((offset + index) & 7))) & 1);
  }
  return value;
}
