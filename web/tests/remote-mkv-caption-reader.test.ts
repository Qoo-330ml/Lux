import { describe, expect, it, vi } from "vitest";
import { RemoteMkvCaptionReader } from "../src/features/player/remote-mkv-caption-reader";

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
  for (let index = bytes - 1; index >= 0; index -= 1) {
    result[index] = value & 0xff;
    value >>>= 8;
  }
  return result;
}

function text(value: string) { return new TextEncoder().encode(value); }
function concat(...parts: Uint8Array[]) { return Uint8Array.from(parts.flatMap((part) => [...part])); }

function makeFixture() {
  const videoTrack = element([0xae], concat(
    element([0xd7], uint(1)),
    element([0x83], uint(1)),
    element([0x86], text("V_MPEG4/ISO/AVC")),
    element([0x63, 0xa2], new Uint8Array([1, 2, 3])),
    element([0xe0], concat(element([0xb0], uint(16)), element([0xba], uint(16)))),
  ));
  const subtitleTrack = element([0xae], concat(
    element([0xd7], uint(2)),
    element([0x73, 0xc5], uint(42, 2)),
    element([0x83], uint(17)),
    element([0x86], text("S_TEXT/UTF8")),
    element([0x53, 0x6e], text("中文")),
    element([0x22, 0xb5, 0x9d], text("zh-CN")),
    element([0x88], uint(1)),
  ));
  const tracks = element([0x16, 0x54, 0xae, 0x6b], concat(videoTrack, subtitleTrack));
  const block = concat(new Uint8Array([0x82, 0, 0, 0]), text("你好"));
  const cluster = element([0x1f, 0x43, 0xb6, 0x75], concat(
    element([0xe7], uint(0)),
    element([0xa0], concat(element([0xa1], block), element([0x9b], uint(2_000, 2)))),
  ));
  const info = element([0x15, 0x49, 0xa9, 0x66], element([0x2a, 0xd7, 0xb1], uint(1_000_000, 4)));
  const seekHeadSize = element([0x11, 0x4d, 0x9b, 0x74], element([0x4d, 0xbb], concat(
    element([0x53, 0xab], new Uint8Array([0x1c, 0x53, 0xbb, 0x6b])),
    element([0x53, 0xac], uint(0, 4)),
  ))).byteLength;
  const cuesPosition = seekHeadSize + info.byteLength + tracks.byteLength + cluster.byteLength;
  const seekHead = element([0x11, 0x4d, 0x9b, 0x74], element([0x4d, 0xbb], concat(
    element([0x53, 0xab], new Uint8Array([0x1c, 0x53, 0xbb, 0x6b])),
    element([0x53, 0xac], uint(cuesPosition, 4)),
  )));
  const clusterPosition = seekHead.byteLength + info.byteLength + tracks.byteLength;
  const cues = element([0x1c, 0x53, 0xbb, 0x6b], element([0xbb], concat(
    element([0xb3], uint(0)),
    element([0xb7], concat(element([0xf7], uint(1)), element([0xf1], uint(clusterPosition, 4)))),
  )));
  const segmentPayload = concat(seekHead, info, tracks, cluster, cues);
  const segment = element([0x18, 0x53, 0x80, 0x67], segmentPayload);
  const ebml = element([0x1a, 0x45, 0xdf, 0xa3], new Uint8Array());
  const segmentDataOffset = ebml.byteLength + 4 + vint(segmentPayload.byteLength).byteLength;
  const source = concat(ebml, segment);
  return { source, cuesOffset: segmentDataOffset + cuesPosition, clusterOffset: segmentDataOffset + clusterPosition };
}

describe("remote Matroska caption sidecar", () => {
  it("reads Cue-selected subtitle clusters without replacing native media playback", async () => {
    const fixture = makeFixture();
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      const range = String(init?.headers && new Headers(init.headers).get("Range"));
      const match = /^bytes=(\d+)-(\d+)$/.exec(range);
      if (!match) throw new Error("missing range");
      const start = Number(match[1]);
      const requestedEnd = Number(match[2]);
      const end = Math.min(fixture.source.byteLength - 1, requestedEnd);
      return new Response(fixture.source.slice(start, end + 1), {
        status: 206,
        headers: {
          "Content-Range": `bytes ${start}-${end}/${fixture.source.byteLength}`,
          ETag: "fixture",
        },
      });
    });
    vi.stubGlobal("fetch", fetchMock);
    const cues: Array<{ trackId: string; text: string }> = [];
    const reader = new RemoteMkvCaptionReader({
      source: "/api/v1/playback/sessions/test/range",
      selection: { id: "2", name: "中文", language: "zh-cn", format: "srt", ordinal: 0 },
      currentTime: () => 0,
      onCue: (cue) => cues.push({ trackId: cue.trackId, text: cue.text }),
    });

    await reader.start();
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(cues).toEqual([{ trackId: "2", text: "你好" }]);
    expect(fetchMock).toHaveBeenCalledTimes(3);
    expect(fetchMock.mock.calls.map((call) => new Headers(call[1]?.headers).get("Range"))).toEqual([
      "bytes=0-1048575",
      `bytes=${fixture.cuesOffset}-${fixture.source.byteLength - 1}`,
      `bytes=${fixture.clusterOffset}-${fixture.source.byteLength - 1}`,
    ]);
    reader.destroy();
  });
});
