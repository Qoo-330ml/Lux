import type { MatroskaTrack } from "./matroska-demuxer";

export function isSupportedMatroskaVideo(track: Pick<MatroskaTrack, "codecId" | "codecPrivate">) {
  return track.codecId.toUpperCase() === "V_MPEGH/ISO/HEVC" && track.codecPrivate.byteLength > 0;
}

export function isSupportedMatroskaAudio(track: Pick<MatroskaTrack, "codecId" | "codecPrivate">) {
  const codec = track.codecId.toUpperCase();
  const audioObjectType = (track.codecPrivate[0] ?? 0) >> 3;
  return codec.startsWith("A_AAC") && audioObjectType === 2 && track.codecPrivate.byteLength >= 2;
}

export function matroskaAudioConfig(track: Pick<MatroskaTrack, "codecId" | "codecPrivate" | "sampleRate" | "channels">) {
  if (!isSupportedMatroskaAudio(track) || !track.sampleRate || !track.channels) return null;
  return {
    codec: "mp4a.40.2",
    asc: track.codecPrivate.slice(),
    sampleRate: track.sampleRate,
    channels: track.channels,
  };
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
