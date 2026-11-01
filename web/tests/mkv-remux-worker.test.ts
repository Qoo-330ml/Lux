/**
 * Investigation harness for the Matroska → fMP4 remux worker path (`hevc-remux` mode).
 *
 * Safari plays HEVC natively but cannot open Matroska, and Chromium opens some Matroska files it
 * still cannot decode. In both cases the client path is the remux worker (no WASM decoder), so these
 * tests drive the *real* worker module over synthetic Matroska fixtures: a codec string, a missing
 * init segment or a hang in the remux wiring shows up as a failing test instead of a player stuck on
 * “正在准备客户端解码…”.
 */
import { afterEach, describe, expect, it, vi } from "vitest";

type WorkerMessage =
  | { type: "init"; initSegment: ArrayBuffer; codec: string }
  | { type: "segment"; mediaSegment: ArrayBuffer; mediaDurationMs: number; processingDurationMs: number }
  | { type: "ready" }
  | { type: "done" }
  | { type: "caption-track"; trackId: string }
  | { type: "caption"; trackId: string }
  | { type: "error"; message: string };

type Scope = typeof globalThis & {
  postMessage?: (message: WorkerMessage, transfer?: unknown[]) => void;
  onmessage?: ((event: { data: unknown }) => void) | null;
};

const scope = globalThis as Scope;
let postToWorker: ((data: unknown) => void) | null = null;

function vint(value: number) {
  if (value < 0x7f) return new Uint8Array([0x80 | value]);
  if (value < 0x3fff) return new Uint8Array([0x40 | (value >> 8), value & 0xff]);
  return new Uint8Array([0x20 | (value >> 16), (value >> 8) & 0xff, value & 0xff]);
}

function element(id: number[], payload: Uint8Array) {
  return new Uint8Array([...id, ...vint(payload.byteLength), ...payload]);
}

function uint(value: number, bytes = 1) {
  const result = new Uint8Array(bytes);
  const view = new DataView(result.buffer);
  if (bytes === 1) view.setUint8(0, value);
  else if (bytes === 2) view.setUint16(0, value);
  else if (bytes === 3) {
    view.setUint8(0, (value >> 16) & 0xff);
    view.setUint16(1, value & 0xffff);
  } else view.setUint32(0, value);
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

/** hvcC for a 3840×2160 HEVC Main 10 source: profile_idc 2, level_idc 153 (5.1). */
function main10HvcC() {
  const codecPrivate = new Uint8Array(23);
  codecPrivate[0] = 1;
  codecPrivate[1] = 0x02;
  codecPrivate[12] = 153;
  return codecPrivate;
}

const LENGTH_PREFIXED_NAL = new Uint8Array([0, 0, 0, 8, 0x40, 0x01, 0x0c, 0x01, 0xff, 0, 0, 0]);
const VP9_FRAME = new Uint8Array([0x10, 0x20, 0x30, 0x40]);

type VideoFixture = {
  codecId: string;
  codecPrivate: Uint8Array;
  width: number;
  height: number;
  /** Frame payload style: length-prefixed NAL units or an opaque codec frame. */
  sample: "nal" | "frame";
};

/**
 * One Matroska file with a video track, an AAC track and ~2.4 s of samples, which is enough for the
 * worker's 2 s fragment batching to flush a first segment.
 */
function matroskaFixture(video: VideoFixture) {
  const videoTrack = element([0xae], concat(
    element([0xd7], uint(1)),
    element([0x73, 0xc5], uint(1)),
    element([0x83], uint(1)),
    element([0x86], text(video.codecId)),
    element([0x63, 0xa2], video.codecPrivate),
    element([0x23, 0xe3, 0x83], uint(41_666_667, 4)),
    element([0xe0], concat(element([0xb0], uint(video.width, 2)), element([0xba], uint(video.height, 2)))),
  ));
  const audioTrack = element([0xae], concat(
    element([0xd7], uint(2)),
    element([0x73, 0xc5], uint(2)),
    element([0x83], uint(2)),
    element([0x86], text("A_AAC/MPEG4/LC")),
    element([0x63, 0xa2], new Uint8Array([0x12, 0x10])),
    element([0xe1], concat(element([0xb5], float(48_000)), element([0x9f], uint(2)))),
  ));
  const blocks: Uint8Array[] = [];
  for (let frame = 0; frame < 60; frame += 1) {
    blocks.push(simpleBlock(1, Math.round(frame * 41.666), frame === 0, video.sample === "nal" ? LENGTH_PREFIXED_NAL : VP9_FRAME));
  }
  for (let frame = 0; frame < 112; frame += 1) {
    blocks.push(simpleBlock(2, Math.round(frame * 21.33), true, new Uint8Array([0xff, 0xf1, 0x50, 0x80, 0x01])));
  }
  return concat(
    element([0x1a, 0x45, 0xdf, 0xa3], new Uint8Array()),
    element([0x18, 0x53, 0x80, 0x67], concat(
      element([0x15, 0x49, 0xa9, 0x66], element([0x2a, 0xd7, 0xb1], uint(1_000_000, 3))),
      element([0x16, 0x54, 0xae, 0x6b], concat(videoTrack, audioTrack)),
      element([0x1f, 0x43, 0xb6, 0x75], concat(element([0xe7], uint(0)), ...blocks)),
    )),
  );
}

function postedText(message: WorkerMessage) {
  if (message.type === "init") return new TextDecoder("latin1").decode(new Uint8Array(message.initSegment));
  if (message.type === "segment") return new TextDecoder("latin1").decode(new Uint8Array(message.mediaSegment));
  return "";
}

async function workerSend() {
  if (postToWorker) return postToWorker;
  await import("../src/features/player/mkv-transcode-worker");
  const handler = scope.onmessage;
  if (typeof handler !== "function") throw new Error("the worker module did not install an onmessage handler");
  postToWorker = (data) => handler({ data });
  return postToWorker;
}

/** Drains the worker's promise chains: it awaits a dynamic `matroska-demuxer` import. */
async function settle() {
  for (let round = 0; round < 10; round += 1) {
    for (let tick = 0; tick < 40; tick += 1) await Promise.resolve();
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

async function remux(fixture: Uint8Array) {
  const posted: WorkerMessage[] = [];
  scope.postMessage = (message) => posted.push(message);
  const send = await workerSend();
  send({ type: "init", wasmUrl: "/hevc/hevc-decode.js", wasmBinaryUrl: "/hevc/hevc-decode.wasm", mode: "hevc-remux" });
  await settle();
  // The engine posts the first 1 MiB Range and the remaining bytes separately.
  send({ type: "data", data: fixture.slice(0, 97).buffer });
  send({ type: "data", data: fixture.slice(97).buffer });
  send({ type: "flush" });
  await settle();
  return posted;
}

describe("MKV remux worker (hevc-remux mode)", () => {
  afterEach(() => {
    scope.postMessage = undefined;
    vi.restoreAllMocks();
  });

  it("emits a Main 10 level 5.1 init segment and a media segment for 4K HEVC", async () => {
    const posted = await remux(matroskaFixture({
      codecId: "V_MPEGH/ISO/HEVC",
      codecPrivate: main10HvcC(),
      width: 3840,
      height: 2160,
      sample: "nal",
    }));

    expect(posted.map((message) => message.type)).toContain("ready");
    expect(posted.filter((message) => message.type === "error")).toEqual([]);
    const init = posted.find((message) => message.type === "init") as Extract<WorkerMessage, { type: "init" }> | undefined;
    // The MSE string must describe the source's real profile, level and audio track.
    expect(init?.codec).toBe("hvc1.2.4.L153.B0,mp4a.40.2");
    const segment = posted.find((message) => message.type === "segment");
    expect(segment && postedText(segment)).toContain("moof");
    expect(segment && postedText(segment)).toContain("mdat");
  });

  const cases = [
    {
      name: "VP9 720p",
      video: {
        codecId: "V_VP9",
        // VP9 Codec Configuration Record (vpcC payload): profile 0, level 3.1, 8-bit 4:2:0.
        codecPrivate: new Uint8Array([1, 0, 0, 0, 0, 0x1f, 0x82, 1, 1, 1, 0, 2, 0xae, 0x01]),
        width: 1280,
        height: 720,
        sample: "frame" as const,
      },
      // Derived from the fixture's VP9 configuration record (profile 0, level 3.1, 8-bit).
      codec: "vp09.00.31.08,mp4a.40.2",
    },
    {
      name: "AV1 480p",
      video: {
        codecId: "V_AV1",
        // AV1CodecConfigurationRecord: marker + version 1, Main profile, level 3.1.
        codecPrivate: new Uint8Array([0x81, 0x00, 0x0c, 0x00]),
        width: 854,
        height: 480,
        sample: "frame" as const,
      },
      // Derived from the fixture's AV1 configuration record (Main profile, level 2.0, 8-bit).
      codec: "av01.0.00M.08,mp4a.40.2",
    },
    {
      name: "H.264 High 8-bit 1080p",
      video: {
        codecId: "V_MPEG4/ISO/AVC",
        // avcC: version 1, profile High (0x64), compat 0x00, level 4.2 (0x2a).
        codecPrivate: new Uint8Array([1, 0x64, 0x00, 0x2a, 0xff, 0xe1, 0x00, 0x04, 0x67, 0x64, 0x00, 0x2a, 0x01, 0x00, 0x04, 0x68, 0xee, 0x3c, 0x80]),
        width: 1920,
        height: 1080,
        sample: "nal" as const,
      },
      codec: "avc1.64002a,mp4a.40.2",
    },
  ];

  for (const testCase of cases) {
    it(`remuxes ${testCase.name} Matroska into fMP4`, async () => {
      const posted = await remux(matroskaFixture(testCase.video));

      expect(posted.filter((message) => message.type === "error")).toEqual([]);
      const init = posted.find((message) => message.type === "init") as Extract<WorkerMessage, { type: "init" }> | undefined;
      expect(init?.codec).toBe(testCase.codec);
      const segment = posted.find((message) => message.type === "segment");
      expect(segment).toBeDefined();
      expect(segment && postedText(segment)).toContain("mdat");
    });
  }
});

type Box = { type: string; start: number; size: number; bodyStart: number; bodyEnd: number };

function readBoxes(data: Uint8Array, start = 0, end = data.byteLength): Box[] {
  const boxes: Box[] = [];
  const view = new DataView(data.buffer, data.byteOffset, data.byteLength);
  let offset = start;
  while (offset + 8 <= end) {
    let size = view.getUint32(offset);
    const type = new TextDecoder("latin1").decode(data.subarray(offset + 4, offset + 8));
    let header = 8;
    if (size === 1) {
      size = Number(view.getBigUint64(offset + 8));
      header = 16;
    } else if (size === 0) {
      size = end - offset;
    }
    if (size < header || offset + size > end) break;
    boxes.push({ type, start: offset, size, bodyStart: offset + header, bodyEnd: offset + size });
    offset += size;
  }
  return boxes;
}

function child(data: Uint8Array, parent: Box, type: string) {
  return readBoxes(data, parent.bodyStart, parent.bodyEnd).find((box) => box.type === type) ?? null;
}

/** Skip the fixed `stsd` header and return every sample entry box. */
function sampleEntries(data: Uint8Array, stsd: Box) {
  const view = new DataView(data.buffer, data.byteOffset, data.byteLength);
  const count = view.getUint32(stsd.bodyStart + 4);
  const entries: Box[] = [];
  let offset = stsd.bodyStart + 8;
  for (let index = 0; index < count; index += 1) {
    const size = view.getUint32(offset);
    const type = new TextDecoder("latin1").decode(data.subarray(offset + 4, offset + 8));
    // VisualSampleEntry (70 bytes) and AudioSampleEntry (20 bytes) follow the 8-byte SampleEntry
    // fields, and the child boxes start after them.
    const fixed = /^(hvc1|hev1|avc1|av01|vp09)$/.test(type) ? 78 : 28;
    entries.push({
      type,
      start: offset,
      size,
      bodyStart: offset + 8 + fixed,
      bodyEnd: offset + size,
    });
    offset += size;
  }
  return entries;
}

/** Walks the documented `moov`/`trak`/`mdia`/`minf`/`stbl`/`stsd` path, then the sample entries. */
function sampleEntryChild(data: Uint8Array, moov: Box, type: string) {
  for (const trak of readBoxes(data, moov.bodyStart, moov.bodyEnd).filter((box) => box.type === "trak")) {
    const stsd = findBoxDeep(data, trak, "stsd");
    if (!stsd) continue;
    for (const entry of sampleEntries(data, stsd)) {
      const found = readBoxes(data, entry.bodyStart, entry.bodyEnd).find((box) => box.type === type);
      if (found) return found;
    }
  }
  return null;
}

function findBoxDeep(data: Uint8Array, box: Box, type: string): Box | null {
  const direct = child(data, box, type);
  if (direct) return direct;
  for (const nested of readBoxes(data, box.bodyStart, box.bodyEnd)) {
    const found = findBoxDeep(data, nested, type);
    if (found) return found;
  }
  return null;
}

describe("MKV remux worker fMP4 structure", () => {
  afterEach(() => {
    scope.postMessage = undefined;
    vi.restoreAllMocks();
  });

  it("writes a fragment Chromium can hand to a 4K HEVC SourceBuffer", async () => {
    const posted = await remux(matroskaFixture({
      codecId: "V_MPEGH/ISO/HEVC",
      codecPrivate: main10HvcC(),
      width: 3840,
      height: 2160,
      sample: "nal",
    }));
    const init = posted.find((message) => message.type === "init") as Extract<WorkerMessage, { type: "init" }>;
    const segment = posted.find((message) => message.type === "segment") as Extract<WorkerMessage, { type: "segment" }>;

    const initBytes = new Uint8Array(init.initSegment);
    const initBoxes = readBoxes(initBytes);
    expect(initBoxes.map((box) => box.type)).toEqual(["ftyp", "moov"]);
    const moov = initBoxes.find((box) => box.type === "moov")!;
    const hvcC = sampleEntryChild(initBytes, moov, "hvcC");
    expect(hvcC).not.toBeNull();
    // The sample description must carry the untouched hvcC, and the codec string must describe the
    // same profile/level the box declares.
    expect([...initBytes.subarray(hvcC!.bodyStart, hvcC!.bodyEnd)]).toEqual([...main10HvcC()]);
    expect(init.codec.startsWith(`hvc1.${main10HvcC()[1] & 0x1f}.`)).toBe(true);
    expect(init.codec).toContain(`L${main10HvcC()[12]}.`);
    const mvex = child(initBytes, moov, "mvex");
    expect(mvex).not.toBeNull();
    const trex = child(initBytes, mvex!, "trex");
    expect(trex).not.toBeNull();

    const segmentBytes = new Uint8Array(segment.mediaSegment);
    const segmentBoxes = readBoxes(segmentBytes);
    expect(segmentBoxes.map((box) => box.type)).toEqual(["moof", "mdat"]);
    const moof = segmentBoxes[0];
    const traf = child(segmentBytes, moof, "traf")!;
    const tfdt = child(segmentBytes, traf, "tfdt")!;
    const trun = child(segmentBytes, traf, "trun")!;
    const tfhd = child(segmentBytes, traf, "tfhd")!;
    expect(tfdt).not.toBeNull();
    expect(trun).not.toBeNull();
    expect(tfhd).not.toBeNull();

    const trunView = new DataView(segmentBytes.buffer, segmentBytes.byteOffset);
    const trunFlags = trunView.getUint32(trun.bodyStart) & 0xffffff;
    const sampleCount = trunView.getUint32(trun.bodyStart + 4);
    // `tr_flags` bits: 0x001 data-offset, 0x004 first-sample-flags, 0x100 duration, 0x200 size,
    // 0x400 sample-flags, 0x800 composition offset.
    const dataOffset = (trunFlags & 0x000001) ? trunView.getInt32(trun.bodyStart + 8) : 0;
    const entryStart = trun.bodyStart + 8 + (trunFlags & 0x000001 ? 4 : 0) + (trunFlags & 0x000004 ? 4 : 0);
    const firstSampleFlags = (trunFlags & 0x000004)
      ? trunView.getUint32(trun.bodyStart + 8 + (trunFlags & 0x000001 ? 4 : 0))
      : (trunFlags & 0x000400)
        ? trunView.getUint32(entryStart + (trunFlags & 0x000100 ? 4 : 0) + (trunFlags & 0x000200 ? 4 : 0))
        : 0;
    // The first sample of a fragment must be a sync sample, otherwise a decoder never starts.
    expect(firstSampleFlags & 0x0001_0000).toBe(0);
    expect(sampleCount).toBeGreaterThan(0);
    // `data_offset` is relative to the start of `moof` and must land on the `mdat` payload.
    const mdat = segmentBoxes[1];
    expect(dataOffset).toBeGreaterThan(0);
    expect(moof.start + dataOffset).toBe(mdat.bodyStart);
  });
});
