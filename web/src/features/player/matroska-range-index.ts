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

export class MatroskaIndexError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "MatroskaIndexError";
  }
}

type Element = { id: number; start: number; dataStart: number; dataEnd: number; end: number; unknown: boolean };

export function parseMatroskaRangeIndex(data: Uint8Array, totalLength = data.byteLength): MatroskaRangeIndex {
  if (totalLength < data.byteLength) throw new MatroskaIndexError("Matroska 索引长度不一致");
  const segment = findElement(data, 0, data.byteLength, IDS.segment);
  if (!segment) throw new MatroskaIndexError("Matroska 缺少 Segment");
  const segmentEnd = segment.unknown ? null : segment.dataEnd;
  const seekHead = findDirectChild(data, segment.dataStart, Math.min(segment.dataEnd, data.byteLength), IDS.seekHead);
  if (!seekHead) throw new MatroskaIndexError("Matroska 缺少有效 SeekHead");
  const seekEntries = parseSeekHead(data, seekHead.dataStart, seekHead.dataEnd);
  const cuesPosition = seekEntries.find((entry) => entry.id === IDS.cues)?.position;
  if (cuesPosition === undefined) throw new MatroskaIndexError("SeekHead 缺少 Cues 位置");
  const cuesOffset = segment.dataStart + cuesPosition;
  if (!Number.isSafeInteger(cuesOffset) || cuesOffset < segment.dataStart || cuesOffset >= totalLength) {
    throw new MatroskaIndexError("Cues 偏移越界");
  }
  const cues = findElement(data, cuesOffset, data.byteLength, IDS.cues);
  if (!cues || cues.dataEnd > totalLength) throw new MatroskaIndexError("Cues 尚未完整读取");
  const parsed = parseCues(data, cues.dataStart, cues.dataEnd, segment.dataStart, totalLength);
  if (parsed.length === 0) throw new MatroskaIndexError("Cues 不包含关键帧位置");
  return {
    segmentOffset: segment.start,
    segmentDataOffset: segment.dataStart,
    segmentEnd,
    cuesOffset,
    cuesEnd: cues.end,
    cues: parsed,
  };
}

export function cueForTime(index: MatroskaRangeIndex, timecode: number, track: number) {
  let candidate: MatroskaCue | null = null;
  for (const cue of index.cues) {
    if (cue.track !== track || cue.timecode > timecode) continue;
    if (!candidate || cue.timecode > candidate.timecode) candidate = cue;
  }
  return candidate;
}

function parseSeekHead(data: Uint8Array, start: number, end: number) {
  const entries: Array<{ id: number; position: number }> = [];
  forEachElement(data, start, end, (element) => {
    if (element.id !== IDS.seek) return;
    let id: number | null = null;
    let position: number | null = null;
    forEachElement(data, element.dataStart, element.dataEnd, (child) => {
      if (child.id === IDS.seekId) id = readUnsigned(data, child.dataStart, child.dataEnd);
      if (child.id === IDS.seekPosition) position = readUnsigned(data, child.dataStart, child.dataEnd);
    }, 1);
    if (id !== null && position !== null) entries.push({ id, position });
  }, 1);
  return entries;
}

function parseCues(data: Uint8Array, start: number, end: number, segmentDataOffset: number, totalLength: number) {
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
      if (track === null || clusterPosition === null || !Number.isSafeInteger(clusterPosition)) return;
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

function findDirectChild(data: Uint8Array, start: number, end: number, id: number) {
  return findElement(data, start, end, id, 1);
}

function findElement(data: Uint8Array, start: number, end: number, id: number, depth = 0): Element | null {
  let found: Element | null = null;
  forEachElement(data, start, end, (element) => {
    if (found === null && element.id === id) found = element;
  }, depth);
  return found;
}

function forEachElement(data: Uint8Array, start: number, end: number, callback: (element: Element) => void, depth: number) {
  if (depth > MAX_DEPTH) throw new MatroskaIndexError("EBML 嵌套层级超限");
  let offset = start;
  while (offset < end) {
    const element = readElement(data, offset, end);
    if (!element || element.end <= offset) throw new MatroskaIndexError("EBML 元素边界无效");
    callback(element);
    offset = element.end;
  }
  if (offset !== end) throw new MatroskaIndexError("EBML 元素未对齐");
}

function readElement(data: Uint8Array, offset: number, end: number): Element | null {
  const id = readVint(data, offset, end, false);
  if (!id) return null;
  const size = readVint(data, id.next, end, true);
  if (!size) return null;
  const dataStart = size.next;
  const dataEnd = size.unknown ? end : dataStart + size.value;
  if (dataEnd > end || dataEnd < dataStart) throw new MatroskaIndexError("EBML 元素越界");
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
