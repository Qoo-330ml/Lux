import { describe, expect, it, vi } from "vitest";
import { MatroskaRangeError, MatroskaRangeReader, parseContentRange } from "../src/features/player/matroska-range-reader";

function response(start: number, end: number, total: number, body = new Uint8Array(end - start + 1), etag = "v1") {
  return new Response(new Uint8Array(body), {
    status: 206,
    headers: { "content-range": `bytes ${start}-${end}/${total}`, etag },
  });
}

describe("Matroska range reader", () => {
  it("starts at one MiB, continues with bounded serial ranges, and never sends HEAD", async () => {
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      const range = String(new Headers(init?.headers).get("range"));
      if (range === "bytes=0-1048575") return response(0, 2, 3, new Uint8Array([1, 2, 3]));
      throw new Error(`unexpected range ${range}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    const chunks: number[] = [];
    for await (const chunk of new MatroskaRangeReader("https://media.invalid/a.mkv").chunks()) chunks.push(...chunk.data);
    expect(chunks).toEqual([1, 2, 3]);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(fetchMock.mock.calls[0][1]?.method).not.toBe("HEAD");
    vi.unstubAllGlobals();
  });

  it("rejects ignored Range responses, missing Content-Range, body length mismatches, and ETag changes", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => new Response(new Uint8Array([1]), { status: 200 })));
    const consume = async () => { for await (const _ of new MatroskaRangeReader("https://media.invalid/a").chunks()) { /* consume */ } };
    await expect(consume()).rejects.toThrow(MatroskaRangeError);
    vi.stubGlobal("fetch", vi.fn(async () => response(0, 1, 2, new Uint8Array([1]), "v1")));
    const consumeShort = async () => { for await (const _ of new MatroskaRangeReader("https://media.invalid/a").chunks()) { /* consume */ } };
    await expect(consumeShort()).rejects.toThrow("Range 长度不一致");
    expect(parseContentRange("bytes 2-1/3")).toBeNull();
    vi.unstubAllGlobals();
  });
});
