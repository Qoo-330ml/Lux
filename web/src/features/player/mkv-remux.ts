import { Box, createFile } from "mp4box";
import { copyBytes, type MatroskaTrack } from "./matroska-demuxer";

type RemuxFile = ReturnType<typeof createFile>;

export function addHevcTrack(file: RemuxFile, codecPrivate: Uint8Array, width: number, height: number) {
  const trackId = file.addTrack({
    type: "hvc1",
    width,
    height,
    timescale: 90_000,
    hdlr: "vide",
  });
  if (!trackId) throw new Error("MKV fMP4 remux 创建视频轨失败");
  const track = file.moov.traks.find((entry) => entry.tkhd.track_id === trackId);
  const sampleEntry = track?.mdia.minf.stbl.stsd.entries[0];
  if (!sampleEntry) throw new Error("MKV fMP4 remux 缺少视频样本描述");
  const hvcC = new Box();
  hvcC.type = "hvcC";
  hvcC.data = copyBytes(codecPrivate);
  sampleEntry.addBox(hvcC);
  return trackId;
}

export function addMatroskaVideoTrack(file: RemuxFile, track: Pick<MatroskaTrack, "codecId" | "codecPrivate" | "width" | "height">) {
  const codec = track.codecId.toUpperCase();
  const type = codec === "V_MPEG4/ISO/AVC" ? "avc1" : codec === "V_VP9" ? "vp09" : codec === "V_AV1" ? "av01" : "hvc1";
  const trackId = file.addTrack({ type, width: track.width ?? 320, height: track.height ?? 320, timescale: 90_000, hdlr: "vide" });
  if (!trackId) throw new Error("MKV fMP4 创建视频轨失败");
  const entry = file.moov.traks.find((item) => item.tkhd.track_id === trackId)?.mdia.minf.stbl.stsd.entries[0];
  if (!entry) throw new Error("MKV fMP4 缺少视频样本描述");
  const description = new Box();
  description.type = type === "avc1" ? "avcC" : type === "hvc1" ? "hvcC" : type === "vp09" ? "vpcC" : "av1C";
  description.data = copyBytes(track.codecPrivate);
  entry.addBox(description);
  return trackId;
}

/**
 * The MSE codec string for a Matroska video track, derived from the track's own configuration
 * record. The remux worker passes this exact string to `addSourceBuffer`, so an 8-bit fallback for a
 * 10-bit stream would create a SourceBuffer the browser cannot decode: the picture stays black while
 * the (separately appended) audio keeps playing.
 */
export function matroskaVideoCodecString(track: Pick<MatroskaTrack, "codecId" | "codecPrivate">) {
  const codec = track.codecId.toUpperCase();
  if (codec === "V_MPEG4/ISO/AVC") {
    const config = track.codecPrivate;
    if (config.byteLength >= 4 && config[0] === 1) {
      return `avc1.${[config[1], config[2], config[3]].map((value) => value.toString(16).padStart(2, "0")).join("")}`;
    }
    return "avc1.640028";
  }
  if (codec === "V_VP9") return vp9CodecString(track.codecPrivate);
  if (codec === "V_AV1") return av1CodecString(track.codecPrivate);
  return hevcCodecString(track.codecPrivate);
}

/** `vp09.<profile>.<level>.<bitDepth>` from the Matroska VP9 configuration record. */
export function vp9CodecString(codecPrivate: Uint8Array) {
  // version (1) + flags (3) + profile (1) + level (1) + depth/chroma byte (1)
  if (codecPrivate.byteLength < 7) return "vp09.00.10.08";
  const profile = codecPrivate[4];
  const level = codecPrivate[5];
  const bitDepth = (codecPrivate[6] >> 4) & 0x0f;
  return `vp09.${pad2(profile)}.${pad2(level)}.${pad2(bitDepth || 8)}`;
}

/** `av01.<profile>.<level><tier>.<bitDepth>` from the AV1 codec configuration record. */
export function av1CodecString(codecPrivate: Uint8Array) {
  if (codecPrivate.byteLength < 3) return "av01.0.04M.08";
  const profile = (codecPrivate[1] >> 5) & 0x07;
  const level = codecPrivate[1] & 0x1f;
  const tier = (codecPrivate[2] & 0x80) ? "H" : "M";
  const highBitDepth = (codecPrivate[2] & 0x40) !== 0;
  const twelveBit = (codecPrivate[2] & 0x20) !== 0;
  const bitDepth = twelveBit ? 12 : highBitDepth ? 10 : 8;
  return `av01.${profile}.${pad2(level)}${tier}.${pad2(bitDepth)}`;
}

function pad2(value: number) {
  return String(Math.max(0, Math.round(value))).padStart(2, "0");
}

export function hevcCodecString(codecPrivate: Uint8Array) {
  const profile = (codecPrivate[1] ?? 1) & 0x1f;
  const compatibility = profile === 2 ? 4 : profile === 1 ? 6 : 0;
  const tier = (codecPrivate[1] ?? 0) & 0x20 ? "H" : "L";
  const level = codecPrivate[12] ?? 120;
  return `hvc1.${profile}.${compatibility}.${tier}${level}.B0`;
}

export function matroskaTimestampTicks(timestampMs: number, timescale: number) {
  return Math.max(0, Math.round(timestampMs * timescale / 1000));
}

export function makeAacEsdsData(asc: Uint8Array) {
  const decoderSpecificInfo = descriptor(5, asc);
  const decoderConfig = descriptor(4, new Uint8Array([0x40, 0x15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]), decoderSpecificInfo);
  const slConfig = descriptor(6, new Uint8Array([2]));
  const elementaryStream = descriptor(3, new Uint8Array([0, 0, 0]), decoderConfig, slConfig);
  return concat(new Uint8Array([0, 0, 0, 0]), elementaryStream);
}

export function concatBuffers(buffers: ArrayBuffer[]) {
  const total = buffers.reduce((sum, buffer) => sum + buffer.byteLength, 0);
  const output = new Uint8Array(total);
  let offset = 0;
  for (const buffer of buffers) {
    output.set(new Uint8Array(buffer), offset);
    offset += buffer.byteLength;
  }
  return output.buffer;
}

export function toLengthPrefixed(data: Uint8Array) {
  if (!startsWithStartCode(data)) return data;
  const nalUnits: Uint8Array[] = [];
  let offset = 0;
  while (offset < data.byteLength) {
    const startCode = findStartCode(data, offset);
    if (!startCode || startCode.index !== offset) return data;
    offset += startCode.length;
    const nextStart = findStartCode(data, offset);
    const end = nextStart?.index ?? data.byteLength;
    if (end <= offset) return data;
    nalUnits.push(data.subarray(offset, end));
    offset = end;
  }
  const total = nalUnits.reduce((sum, nal) => sum + 4 + nal.byteLength, 0);
  const output = new Uint8Array(total);
  let writeOffset = 0;
  for (const nal of nalUnits) {
    new DataView(output.buffer).setUint32(writeOffset, nal.byteLength);
    writeOffset += 4;
    output.set(nal, writeOffset);
    writeOffset += nal.byteLength;
  }
  return output;
}

function descriptor(tag: number, ...payloads: Uint8Array[]) {
  const payload = concat(...payloads);
  const sizeBytes: number[] = [];
  let length = payload.byteLength;
  do {
    sizeBytes.unshift(length & 0x7f);
    length >>>= 7;
  } while (length > 0);
  for (let index = 0; index < sizeBytes.length - 1; index += 1) sizeBytes[index] |= 0x80;
  return concat(new Uint8Array([tag, ...sizeBytes]), payload);
}

function concat(...arrays: Uint8Array[]) {
  const total = arrays.reduce((sum, array) => sum + array.byteLength, 0);
  const output = new Uint8Array(total);
  let offset = 0;
  for (const array of arrays) {
    output.set(array, offset);
    offset += array.byteLength;
  }
  return output;
}

function startsWithStartCode(data: Uint8Array) {
  return data.byteLength >= 4 && data[0] === 0 && data[1] === 0 && (data[2] === 1 || data[2] === 0 && data[3] === 1);
}

function findStartCode(data: Uint8Array, start: number) {
  for (let index = start; index + 3 < data.byteLength; index += 1) {
    if (data[index] !== 0 || data[index + 1] !== 0) continue;
    if (data[index + 2] === 1) return { index, length: 3 };
    if (data[index + 2] === 0 && data[index + 3] === 1) return { index, length: 4 };
  }
  return null;
}
