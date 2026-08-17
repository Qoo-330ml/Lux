import { Decoder, type EbmlElement } from "ebml";

const IDS = {
  ebml: 0x1a45dfa3,
  segment: 0x18538067,
  info: 0x1549a966,
  timecodeScale: 0x2ad7b1,
  tracks: 0x1654ae6b,
  trackEntry: 0xae,
  trackNumber: 0xd7,
  trackType: 0x83,
  codecId: 0x86,
  codecPrivate: 0x63a2,
  defaultDuration: 0x23e383,
  video: 0xe0,
  pixelWidth: 0xb0,
  pixelHeight: 0xba,
  audio: 0xe1,
  samplingFrequency: 0xb5,
  channels: 0x9f,
  cluster: 0x1f43b675,
  clusterTimecode: 0xe7,
  simpleBlock: 0xa3,
  block: 0xa1,
} as const;

type TrackType = "video" | "audio" | "other";

export type MatroskaTrack = {
  number: number;
  type: TrackType;
  codecId: string;
  codecPrivate: Uint8Array;
  defaultDurationMs: number | null;
  width: number | null;
  height: number | null;
  sampleRate: number | null;
  channels: number | null;
};

export type MatroskaSample = {
  trackNumber: number;
  timestampMs: number;
  durationMs: number;
  keyframe: boolean;
  data: Uint8Array;
};

export type MatroskaFile = {
  timecodeScale: number;
  videoTrack: MatroskaTrack | null;
  audioTrack: MatroskaTrack | null;
  videoSamples: MatroskaSample[];
  audioSamples: MatroskaSample[];
};

export type MatroskaStreamCallbacks = {
  onTrack?: (track: MatroskaTrack) => void;
  onSample?: (sample: MatroskaSample) => void;
  onTimecodeScale?: (scale: number) => void;
  onError?: (error: Error) => void;
};

type Element = { id: number; dataStart: number; dataEnd: number; end: number };

export function parseMatroska(data: Uint8Array): MatroskaFile {
  const result: MatroskaFile = {
    timecodeScale: 1_000_000,
    videoTrack: null,
    audioTrack: null,
    videoSamples: [],
    audioSamples: [],
  };
  parseRange(data, 0, data.byteLength, result, null, null, null);
  finalizeDurations(result.videoSamples, result.videoTrack?.defaultDurationMs ?? null);
  finalizeDurations(result.audioSamples, result.audioTrack?.defaultDurationMs ?? null);
  return result;
}

export class MatroskaStreamDemuxer {
  private readonly decoder: Decoder;
  private readonly callbacks: MatroskaStreamCallbacks;
  private readonly tracks = new Map<number, MatroskaTrack>();
  private path: string[] = [];
  private currentTrack: Partial<MatroskaTrack> | null = null;
  private clusterTimecode = 0;
  private timecodeScale = 1_000_000;
  private pendingBlocks: Array<{ data: Uint8Array; simple: boolean }> = [];

  constructor(callbacks: MatroskaStreamCallbacks) {
    this.callbacks = callbacks;
    this.decoder = new Decoder();
    this.decoder.on("data", (chunk) => this.consume(chunk[0], chunk[1]));
    this.decoder.on("error", (error) => callbacks.onError?.(error));
  }

  write(chunk: Uint8Array) {
    this.decoder.write(chunk);
  }

  end() {
    this.decoder.end();
  }

  private consume(kind: "start" | "tag" | "end", element: EbmlElement) {
    if (kind === "start") {
      this.path.push(element.name);
      if (element.name === "TrackEntry") this.currentTrack = { codecPrivate: new Uint8Array(), defaultDurationMs: null, width: null, height: null, sampleRate: null, channels: null };
      return;
    }
    if (kind === "end") {
      if (element.name === "TrackEntry" && this.currentTrack) this.finishTrack();
      this.path.pop();
      return;
    }
    const bytes = element.data;
    if (!bytes) return;
    if (element.name === "TimecodeScale") {
      const scale = readUnsigned(bytes, 0, bytes.byteLength);
      if (scale) {
        this.timecodeScale = scale;
        this.callbacks.onTimecodeScale?.(scale);
      }
    } else if (element.name === "Timecode" && this.path.includes("Cluster")) {
      this.clusterTimecode = readUnsigned(bytes, 0, bytes.byteLength) ?? 0;
    } else if ((element.name === "SimpleBlock" || element.name === "Block") && this.path.includes("Cluster")) {
      this.consumeBlock(bytes, element.name === "SimpleBlock");
    } else if (this.currentTrack && this.path.includes("TrackEntry")) {
      readTrackFieldByName(element.name, bytes, this.currentTrack);
    }
  }

  private finishTrack() {
    if (!this.currentTrack?.number || !this.currentTrack.type || !this.currentTrack.codecId) {
      this.currentTrack = null;
      return;
    }
    const track = this.currentTrack as MatroskaTrack;
    this.tracks.set(track.number, track);
    this.callbacks.onTrack?.(track);
    this.currentTrack = null;
    const pending = this.pendingBlocks;
    this.pendingBlocks = [];
    pending.forEach((block) => this.consumeBlock(block.data, block.simple));
  }

  private consumeBlock(data: Uint8Array, simple: boolean) {
    const block = parseSimpleBlockPayload(data);
    if (!block) return;
    const track = this.tracks.get(block.trackNumber);
    if (!track) {
      this.pendingBlocks.push({ data: data.slice(), simple });
      return;
    }
    const timestampMs = (this.clusterTimecode + block.timecode) * this.timecodeScale / 1_000_000;
    const defaultDuration = defaultSampleDurationMs(track);
    block.frames.forEach((frame, index) => this.callbacks.onSample?.({
      trackNumber: track.number,
      timestampMs: timestampMs + index * defaultDuration,
      durationMs: defaultDuration,
      keyframe: simple && Boolean(block.flags & 0x80),
      data: frame,
    }));
  }
}

function parseRange(
  data: Uint8Array,
  start: number,
  end: number,
  result: MatroskaFile,
  trackEntry: Partial<MatroskaTrack> | null,
  clusterTimecode: number | null,
  clusterScale: number | null,
) {
  let offset = start;
  let currentClusterTimecode = clusterTimecode;
  let currentClusterScale = clusterScale;
  while (offset < end) {
    const element = readElement(data, offset, end);
    if (!element) return;
    const boundedEnd = Math.min(element.dataEnd, end);
    if (element.id === IDS.ebml || element.id === IDS.segment || element.id === IDS.info) parseRange(data, element.dataStart, boundedEnd, result, trackEntry, clusterTimecode, clusterScale);
    else if (element.id === IDS.timecodeScale) result.timecodeScale = readUnsigned(data, element.dataStart, boundedEnd) || 1_000_000;
    else if (element.id === IDS.tracks) parseRange(data, element.dataStart, boundedEnd, result, trackEntry, clusterTimecode, clusterScale);
    else if (element.id === IDS.trackEntry) parseTrackEntry(data, element.dataStart, boundedEnd, result);
    else if (element.id === IDS.cluster) parseRange(data, element.dataStart, boundedEnd, result, trackEntry, 0, result.timecodeScale);
    else if (element.id === IDS.clusterTimecode && currentClusterTimecode !== null) {
      const value = readUnsigned(data, element.dataStart, boundedEnd);
      if (value !== null) {
        currentClusterTimecode = value;
        currentClusterScale = clusterScale;
      }
    } else if ((element.id === IDS.simpleBlock || element.id === IDS.block) && currentClusterTimecode !== null) {
      parseBlock(data, element.dataStart, boundedEnd, result, currentClusterTimecode, currentClusterScale ?? result.timecodeScale, element.id === IDS.simpleBlock);
    } else if (element.id === IDS.video || element.id === IDS.audio) {
      parseRange(data, element.dataStart, boundedEnd, result, trackEntry, clusterTimecode, clusterScale);
    } else if (trackEntry) readTrackField(data, element, trackEntry);
    offset = element.end;
  }
}

function parseTrackEntry(data: Uint8Array, start: number, end: number, result: MatroskaFile) {
  const track: Partial<MatroskaTrack> = { codecPrivate: new Uint8Array(), defaultDurationMs: null, width: null, height: null, sampleRate: null, channels: null };
  parseRange(data, start, end, result, track, null, null);
  if (!track.number || !track.type || !track.codecId) return;
  const complete = track as MatroskaTrack;
  if (complete.type === "video" && !result.videoTrack) result.videoTrack = complete;
  if (complete.type === "audio" && !result.audioTrack) result.audioTrack = complete;
}

function readTrackField(data: Uint8Array, element: Element, track: Partial<MatroskaTrack>) {
  if (element.id === IDS.trackNumber) track.number = readUnsigned(data, element.dataStart, element.dataEnd) ?? undefined;
  else if (element.id === IDS.trackType) {
    const value = readUnsigned(data, element.dataStart, element.dataEnd);
    track.type = value === 1 ? "video" : value === 2 ? "audio" : "other";
  } else if (element.id === IDS.codecId) track.codecId = new TextDecoder().decode(data.subarray(element.dataStart, element.dataEnd));
  else if (element.id === IDS.codecPrivate) track.codecPrivate = data.slice(element.dataStart, element.dataEnd);
  else if (element.id === IDS.defaultDuration) {
    const value = readUnsigned(data, element.dataStart, element.dataEnd);
    track.defaultDurationMs = value === null ? null : value / 1_000_000;
  } else if (element.id === IDS.pixelWidth) track.width = readUnsigned(data, element.dataStart, element.dataEnd);
  else if (element.id === IDS.pixelHeight) track.height = readUnsigned(data, element.dataStart, element.dataEnd);
  else if (element.id === IDS.samplingFrequency) track.sampleRate = readFloat(data, element.dataStart, element.dataEnd);
  else if (element.id === IDS.channels) track.channels = readUnsigned(data, element.dataStart, element.dataEnd);
}

function readTrackFieldByName(name: string, data: Uint8Array, track: Partial<MatroskaTrack>) {
  if (name === "TrackNumber") track.number = readUnsigned(data, 0, data.byteLength) ?? undefined;
  else if (name === "TrackType") {
    const value = readUnsigned(data, 0, data.byteLength);
    track.type = value === 1 ? "video" : value === 2 ? "audio" : "other";
  } else if (name === "CodecID") track.codecId = new TextDecoder().decode(data);
  else if (name === "CodecPrivate") track.codecPrivate = data.slice();
  else if (name === "DefaultDuration") track.defaultDurationMs = (readUnsigned(data, 0, data.byteLength) ?? 0) / 1_000_000;
  else if (name === "PixelWidth") track.width = readUnsigned(data, 0, data.byteLength);
  else if (name === "PixelHeight") track.height = readUnsigned(data, 0, data.byteLength);
  else if (name === "SamplingFrequency") track.sampleRate = readFloat(data, 0, data.byteLength);
  else if (name === "Channels") track.channels = readUnsigned(data, 0, data.byteLength);
}

function parseBlock(
  data: Uint8Array,
  start: number,
  end: number,
  result: MatroskaFile,
  clusterTimecode: number,
  scale: number,
  simple: boolean,
) {
  const block = parseSimpleBlockPayload(data.slice(start, end));
  if (!block) return;
  const track = findTrack(result, block.trackNumber);
  if (!track) return;
  const timestampMs = (clusterTimecode + block.timecode) * scale / 1_000_000;
  const defaultDuration = defaultSampleDurationMs(track);
  block.frames.forEach((frame, index) => {
    const sample: MatroskaSample = {
      trackNumber: track.number,
      timestampMs: timestampMs + index * defaultDuration,
      durationMs: defaultDuration,
      keyframe: simple && Boolean(block.flags & 0x80),
      data: frame,
    };
    if (track.type === "video") result.videoSamples.push(sample);
    else if (track.type === "audio") result.audioSamples.push(sample);
  });
}

export function parseSimpleBlockPayload(data: Uint8Array) {
  const trackVint = readVint(data, 0, data.byteLength);
  if (!trackVint || trackVint.value === 0 || trackVint.next + 3 > data.byteLength) return null;
  const view = new DataView(data.buffer, data.byteOffset + trackVint.next, 2);
  const timecode = view.getInt16(0);
  const flags = data[trackVint.next + 2];
  return {
    trackNumber: trackVint.value,
    timecode,
    flags,
    frames: splitLacedPayload(data, trackVint.next + 3, data.byteLength, flags),
  };
}

function findTrack(result: MatroskaFile, number: number) {
  return [result.videoTrack, result.audioTrack].find((track) => track?.number === number) ?? null;
}

function defaultSampleDurationMs(track: MatroskaTrack) {
  if (track.defaultDurationMs !== null) return track.defaultDurationMs;
  if (track.type !== "audio" || !track.sampleRate) return 0;
  return track.codecId.toUpperCase() === "A_AC3" ? 1536_000 / track.sampleRate : 1024_000 / track.sampleRate;
}

function splitLacedPayload(data: Uint8Array, start: number, end: number, flags: number) {
  const lacing = (flags >> 1) & 0x03;
  if (lacing === 0) return [data.slice(start, end)];
  if (start >= end) return [];
  const count = data[start] + 1;
  let offset = start + 1;
  if (count <= 0) return [];
  if (lacing === 2) {
    const payloadLength = end - offset;
    const size = Math.floor(payloadLength / count);
    return Array.from({ length: count }, (_, index) => data.slice(offset + index * size, offset + (index + 1) * size));
  }
  const sizes: number[] = [];
  if (lacing === 1) {
    for (let index = 0; index < count - 1; index += 1) {
      let size = 0;
      while (offset < end) {
        const value = data[offset++];
        size += value;
        if (value !== 0xff) break;
      }
      sizes.push(size);
    }
  } else {
    const first = readVint(data, offset, end);
    if (!first) return [];
    sizes.push(first.value);
    offset = first.next;
    for (let index = 1; index < count - 1; index += 1) {
      const next = readSignedVint(data, offset, end);
      if (!next) return [];
      sizes.push(sizes[index - 1] + next.value);
      offset = next.next;
    }
  }
  const frames: Uint8Array[] = [];
  for (const size of sizes) {
    if (size < 0 || offset + size > end) return [];
    frames.push(data.slice(offset, offset + size));
    offset += size;
  }
  if (offset > end) return [];
  frames.push(data.slice(offset, end));
  return frames;
}

function finalizeDurations(samples: MatroskaSample[], fallback: number | null) {
  samples.forEach((sample, index) => {
    if (sample.durationMs > 0) return;
    const next = samples[index + 1];
    sample.durationMs = next ? Math.max(0, next.timestampMs - sample.timestampMs) : fallback ?? 0;
  });
}

function readElement(data: Uint8Array, offset: number, end: number): Element | null {
  const id = readVint(data, offset, end, false);
  if (!id) return null;
  const size = readVint(data, id.next, end);
  if (!size) return null;
  const dataStart = size.next;
  const dataEnd = size.unknown ? end : Math.min(end, dataStart + size.value);
  return { id: id.value, dataStart, dataEnd, end: dataEnd };
}

function readVint(data: Uint8Array, offset: number, end: number, stripMarker = true): { value: number; next: number; unknown?: boolean } | null {
  if (offset >= end) return null;
  const first = data[offset];
  let length = 1;
  let mask = 0x80;
  while (length <= 8 && (first & mask) === 0) {
    length += 1;
    mask >>= 1;
  }
  if (length > 8 || offset + length > end) return null;
  let value = stripMarker ? first & (mask - 1) : first;
  for (let index = 1; index < length; index += 1) value = value * 256 + data[offset + index];
  const unknown = stripMarker && length <= 8 && value === (2 ** (7 * length)) - 1;
  return { value, next: offset + length, unknown };
}

function readSignedVint(data: Uint8Array, offset: number, end: number) {
  const result = readVint(data, offset, end);
  if (!result) return null;
  const width = result.next - offset;
  return { value: result.value - ((2 ** (7 * width - 1)) - 1), next: result.next };
}

function readUnsigned(data: Uint8Array, start: number, end: number) {
  if (start >= end || end - start > 8) return null;
  let value = 0;
  for (let offset = start; offset < end; offset += 1) value = value * 256 + data[offset];
  return value;
}

function readFloat(data: Uint8Array, start: number, end: number) {
  if (end - start === 4) return new DataView(data.buffer, data.byteOffset + start, 4).getFloat32(0);
  if (end - start === 8) return new DataView(data.buffer, data.byteOffset + start, 8).getFloat64(0);
  return null;
}
