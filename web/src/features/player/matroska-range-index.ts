/**
 * Small, bounded Matroska index reader used by the remote client pipeline.
 * It intentionally only reads EBML metadata; Cluster payloads are never
 * scanned while building the index.
 */

const IDS = {
  segment: 0x18538067,
  seekHead: 0x114d9b74,
  seek: 0x4dbb,
  seekId: 0x53ab,
  seekPosition: 0x53ac,
  cues: 0x1c53bb6b,
  cuePoint: 0xbb,
  cueTime: 0xb3,
  cueTrackPositions: 0xb7,
  cueTrack: 0xf7,
  cueClusterPosition: 0xf1,
  cueBlockNumber: 0x5378,
} as const;

const MAX_CUES = 100_000;
const MAX_METADATA_BYTES = 8 * 1024 * 1024;
const MAX_DEPTH = 16;

export type MatroskaCue = {
  timecode: number;
  track: number;
  clusterOffset: number;
  blockNumber: number;
};

export type MatroskaRangeIndex = {
  segmentOffset: number;
  segmentDataOffset: number;
  segmentEnd: number | null;
  cuesOffset: number;
  cuesEnd: number;
  cues: MatroskaCue[];
};

export type MatroskaIndexMetadata = {
  segmentOffset: number;
  segmentDataOffset: number;
  segmentEnd: number | null;
  cuesOffset: number;
};

export class MatroskaIndexError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "MatroskaIndexError";
  }
}

/** Validates the required index anchor without scanning Cluster payloads. */
export function hasMatroskaSeekHead(data: Uint8Array, totalLength = data.byteLength) {
  // The first Range is only a prefix of the file. A finite Segment size may
  // therefore extend beyond the bytes we have, while its SeekHead is already
  // available near the beginning. Keep strict bounds for the full index
  // parser, but allow this lightweight anchor check to inspect a clipped
  // outer Segment element.
  const segment = findElement(data, 0, data.byteLength, IDS.segment, 0, true);
  if (!segment) return false;
  const seekHead = findDirectChild(data, segment.dataStart, Math.min(segment.dataEnd, data.byteLength), IDS.seekHead, true);
  const cuesPosition = seekHead
    ? parseSeekHead(data, seekHead.dataStart, seekHead.dataEnd, true).find((entry) => entry.id === IDS.cues)?.position
    : undefined;
  return cuesPosition !== undefined && Number.isSafeInteger(cuesPosition)
    && segment.dataStart + cuesPosition >= segment.dataStart
    && segment.dataStart + cuesPosition < totalLength;
}

type Element = { id: number; start: number; dataStart: number; dataEnd: number; end: number; unknown: boolean };

export function parseMatroskaRangeIndex(data: Uint8Array, totalLength = data.byteLength, videoTrack?: number): MatroskaRangeIndex {
  if (totalLength < data.byteLength) throw new MatroskaIndexError("Matroska 索引长度不一致");
  const metadata = locateMatroskaIndex(data, totalLength);
  const cuesOffset = metadata.cuesOffset;
  const cues = findElement(data, cuesOffset, data.byteLength, IDS.cues);
  if (!cues || cues.dataEnd > totalLength) throw new MatroskaIndexError("Cues 尚未完整读取");
  if (cues.dataEnd - cues.dataStart > MAX_METADATA_BYTES) throw new MatroskaIndexError("Cues 元数据超限");
  const parsed = parseCues(data, cues.dataStart, cues.dataEnd, metadata.segmentDataOffset, totalLength, videoTrack);
  if (parsed.length === 0) throw new MatroskaIndexError("Cues 不包含关键帧位置");
  return {
    ...metadata,
    cuesOffset,
    cuesEnd: cues.end,
    cues: parsed,
  };
}

/** Locates the Segment-relative Cues position from a bounded metadata range. */
export function locateMatroskaIndex(data: Uint8Array, totalLength = data.byteLength): MatroskaIndexMetadata {
  const segment = findElement(data, 0, data.byteLength, IDS.segment, 0, true);
  if (!segment) throw new MatroskaIndexError("Matroska 缺少 Segment");
  const segmentEnd = segment.unknown ? null : segment.dataEnd;
  const seekHead = findDirectChild(data, segment.dataStart, Math.min(segment.dataEnd, data.byteLength), IDS.seekHead, true);
  if (!seekHead) throw new MatroskaIndexError("Matroska 缺少有效 SeekHead");
  if (seekHead.dataEnd - seekHead.dataStart > MAX_METADATA_BYTES) throw new MatroskaIndexError("SeekHead 元数据超限");
  const cuesPosition = parseSeekHead(data, seekHead.dataStart, seekHead.dataEnd, true).find((entry) => entry.id === IDS.cues)?.position;
  if (cuesPosition === undefined) throw new MatroskaIndexError("SeekHead 缺少 Cues 位置");
  const cuesOffset = segment.dataStart + cuesPosition;
  if (!Number.isSafeInteger(cuesOffset) || cuesOffset < segment.dataStart || cuesOffset >= totalLength) {
    throw new MatroskaIndexError("Cues 偏移越界");
  }
  return {
    segmentOffset: segment.start,
    segmentDataOffset: segment.dataStart,
    segmentEnd,
    cuesOffset,
  };
}

/** Parses a Cues element fetched as a separate absolute Range. */
export function parseMatroskaCuesRange(
  data: Uint8Array,
  rangeStart: number,
  totalLength: number,
  metadata: MatroskaIndexMetadata,
  videoTrack?: number,
) {
  if (!Number.isSafeInteger(rangeStart) || rangeStart < 0 || rangeStart > metadata.cuesOffset) {
    throw new MatroskaIndexError("Cues Range 起点无效");
  }
  const localOffset = metadata.cuesOffset - rangeStart;
  const cues = findElement(data, localOffset, data.byteLength, IDS.cues);
  if (!cues || cues.dataEnd > data.byteLength) throw new MatroskaIndexError("Cues 尚未完整读取");
  if (cues.dataEnd - cues.dataStart > MAX_METADATA_BYTES) throw new MatroskaIndexError("Cues 元数据超限");
  const parsed = parseCues(data, cues.dataStart, cues.dataEnd, metadata.segmentDataOffset, totalLength, videoTrack);
  if (parsed.length === 0) throw new MatroskaIndexError("Cues 不包含关键帧位置");
  return {
    ...metadata,
    cuesEnd: rangeStart + cues.end,
    cues: parsed,
  } satisfies MatroskaRangeIndex;
}

export function cueForTime(index: MatroskaRangeIndex, timecode: number, track: number) {
  let candidate: MatroskaCue | null = null;
  for (const cue of index.cues) {
    if (cue.track !== track || cue.timecode > timecode) continue;
    if (!candidate || cue.timecode > candidate.timecode) candidate = cue;
  }
  return candidate;
}

function parseSeekHead(data: Uint8Array, start: number, end: number, allowTruncated = false) {
  const entries: Array<{ id: number; position: number }> = [];
  forEachElement(data, start, end, (element) => {
    if (element.id !== IDS.seek) return;
    let id: number | null = null;
    let position: number | null = null;
    forEachElement(data, element.dataStart, element.dataEnd, (child) => {
      if (child.id === IDS.seekId) id = readUnsigned(data, child.dataStart, child.dataEnd);
      if (child.id === IDS.seekPosition) position = readUnsigned(data, child.dataStart, child.dataEnd);
    }, 1, allowTruncated);
    if (id !== null && position !== null) entries.push({ id, position });
  }, 1, allowTruncated);
  return entries;
}

function parseCues(data: Uint8Array, start: number, end: number, segmentDataOffset: number, totalLength: number, videoTrack?: number) {
  const cues: MatroskaCue[] = [];
  forEachElement(data, start, end, (point) => {
    if (point.id !== IDS.cuePoint) return;
    let timecode: number | null = null;
    forEachElement(data, point.dataStart, point.dataEnd, (child) => {
      if (child.id === IDS.cueTime) timecode = readUnsigned(data, child.dataStart, child.dataEnd);
      if (child.id !== IDS.cueTrackPositions || timecode === null) return;
      let track: number | null = null;
      let clusterPosition: number | null = null;
      let blockNumber = 1;
      forEachElement(data, child.dataStart, child.dataEnd, (position) => {
        if (position.id === IDS.cueTrack) track = readUnsigned(data, position.dataStart, position.dataEnd);
        if (position.id === IDS.cueClusterPosition) clusterPosition = readUnsigned(data, position.dataStart, position.dataEnd);
        if (position.id === IDS.cueBlockNumber) blockNumber = readUnsigned(data, position.dataStart, position.dataEnd) ?? 1;
      }, 2);
      if (track === null || clusterPosition === null || !Number.isSafeInteger(clusterPosition) || (videoTrack !== undefined && track !== videoTrack)) return;
      const clusterOffset = segmentDataOffset + clusterPosition;
      if (clusterOffset < segmentDataOffset || clusterOffset >= totalLength) {
        throw new MatroskaIndexError("Cue ClusterPosition 越界");
      }
      if (cues.length >= MAX_CUES) throw new MatroskaIndexError("CuePoint 数量超限");
      cues.push({ timecode, track, clusterOffset, blockNumber });
    }, 1);
  }, 1);
  return cues.sort((left, right) => left.timecode - right.timecode || left.track - right.track);
}

function findDirectChild(data: Uint8Array, start: number, end: number, id: number, allowTruncated = false) {
  return findElement(data, start, end, id, 1, allowTruncated);
}

function findElement(data: Uint8Array, start: number, end: number, id: number, depth = 0, allowTruncated = false): Element | null {
  let found: Element | null = null;
  forEachElement(data, start, end, (element) => {
    if (found === null && element.id === id) found = element;
  }, depth, allowTruncated);
  return found;
}

function forEachElement(data: Uint8Array, start: number, end: number, callback: (element: Element) => void, depth: number, allowTruncated = false) {
  if (depth > MAX_DEPTH) throw new MatroskaIndexError("EBML 嵌套层级超限");
  let offset = start;
  while (offset < end) {
    const element = readElement(data, offset, end, allowTruncated);
    if (!element || element.end <= offset) {
      if (allowTruncated) break;
      throw new MatroskaIndexError("EBML 元素边界无效");
    }
    callback(element);
    offset = element.end;
  }
  if (offset !== end && !allowTruncated) throw new MatroskaIndexError("EBML 元素未对齐");
}

function readElement(data: Uint8Array, offset: number, end: number, allowTruncated = false): Element | null {
  const id = readVint(data, offset, end, false);
  if (!id) return null;
  const size = readVint(data, id.next, end, true);
  if (!size) return null;
  const dataStart = size.next;
  const dataEnd = size.unknown ? end : dataStart + size.value;
  if (dataEnd < dataStart) throw new MatroskaIndexError("EBML 元素越界");
  if (dataEnd > end) {
    if (!allowTruncated) throw new MatroskaIndexError("EBML 元素越界");
    return { id: id.value, start: offset, dataStart, dataEnd: end, end, unknown: true };
  }
  return { id: id.value, start: offset, dataStart, dataEnd, end: dataEnd, unknown: Boolean(size.unknown) };
}

function readVint(data: Uint8Array, offset: number, end: number, stripMarker: boolean) {
  if (offset >= end) return null;
  const first = data[offset];
  let mask = 0x80;
  let length = 1;
  while (length <= 8 && (first & mask) === 0) { length += 1; mask >>= 1; }
  if (length > 8 || offset + length > end) return null;
  let value = stripMarker ? first & (mask - 1) : first;
  for (let index = 1; index < length; index += 1) value = value * 256 + data[offset + index];
  return { value, next: offset + length, unknown: stripMarker && value === (2 ** (7 * length)) - 1 };
}

function readUnsigned(data: Uint8Array, start: number, end: number) {
  if (start >= end || end - start > 8) return null;
  let value = 0;
  for (let index = start; index < end; index += 1) value = value * 256 + data[index];
  return Number.isSafeInteger(value) ? value : null;
}
