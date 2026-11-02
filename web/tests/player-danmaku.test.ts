import { describe, expect, it } from "vitest";
import {
  DANMAKU_LIMITS,
  DANMAKU_SERVER_MAX_BYTES,
  danmakuFailureMessage,
  DanmakuParseError,
  decodeDanmakuBytes,
  exceedsUtf8ByteLimit,
  parseBilibiliDanmaku,
  parseDanmakuXml,
  parseDanmakuPayload,
  readDanmakuResponse,
} from "../src/features/player/danmaku";
import { parseDanmakuWorkerRequest } from "../src/features/player/danmaku-worker";

const NO_DANMAKU = '<d p="1,1,25,0,0,0,0,0">entry</d>';

function deterministicRandom(seed = 1) {
  let state = seed >>> 0;
  return () => {
    // mulberry32: a tiny deterministic generator so sampling assertions stay reproducible.
    state = (state + 0x6d2b79f5) >>> 0;
    let value = state;
    value = Math.imul(value ^ (value >>> 15), value | 1);
    value ^= value + Math.imul(value ^ (value >>> 7), value | 61);
    return ((value ^ (value >>> 14)) >>> 0) / 4294967296;
  };
}

describe("LuxPlayer danmaku parser", () => {
  it("checks UTF-8 byte limits without changing Unicode byte semantics", () => {
    expect(exceedsUtf8ByteLimit("ascii", 5)).toBe(false);
    expect(exceedsUtf8ByteLimit("ascii", 4)).toBe(true);
    expect(exceedsUtf8ByteLimit("中文", 5)).toBe(true);
    expect(exceedsUtf8ByteLimit("中文", 6)).toBe(false);
    expect(exceedsUtf8ByteLimit("🙂", 3)).toBe(true);
    expect(exceedsUtf8ByteLimit("🙂", 4)).toBe(false);
    expect(exceedsUtf8ByteLimit("\uD800", 2)).toBe(true);
    expect(exceedsUtf8ByteLimit("\uD800", 3)).toBe(false);
  });

  it("normalizes bounded Bilibili XML into text-only supported modes", () => {
    expect(parseBilibiliDanmaku(
      '<i><d p="2,1,25,16711680,0,0,0,0">&lt;b&gt;滚动&lt;/b&gt;</d><d p="3,5,30,16777215,0,0,0,0">顶部</d><d p="4,4,20,255,0,0,0,0">底部</d></i>',
    )).toEqual([
      { id: "danmaku-0", start: 2, mode: "scroll", text: "<b>滚动</b>", color: "#ff0000", fontSize: 25 },
      { id: "danmaku-1", start: 3, mode: "top", text: "顶部", color: "#ffffff", fontSize: 30 },
      { id: "danmaku-2", start: 4, mode: "bottom", text: "底部", color: "#0000ff", fontSize: 20 },
    ]);
  });

  it("accepts downloaded Bilibili XML with a standard declaration", () => {
    expect(parseBilibiliDanmaku(
      '<?xml version="1.0" encoding="UTF-8"?>\n<i><chatserver>chat.bilibili.com</chatserver><chatid>1</chatid><d p="1,1,25,16777215,0,0,0,0">下载弹幕</d></i>',
    )).toEqual([{
      id: "danmaku-0",
      start: 1,
      mode: "scroll",
      text: "下载弹幕",
      color: "#ffffff",
      fontSize: 25,
    }]);
  });

  it("tolerates eight-field Bilibili p attributes and skips advanced or dirty entries", () => {
    const result = parseDanmakuXml(
      '<i>'
      // Real Bilibili rows carry eight fields; only the first four are meaningful here.
      + '<d p="12.5,1,25,16777215,1700000000,0,deadbeef,65264337">八字段</d>'
      // Mode 7 is an advanced/positioned danmaku the Web renderer does not draw yet.
      + '<d p="13,7,25,0,0,0,0,0">[0,0,"1-1",4,"高级",0,0,0,0,500,0,1,"#fff"]</d>'
      // Mode 8 (code danmaku) and unknown modes are skipped as well.
      + '<d p="14,8,25,0,0,0,0,0">code</d><d p="15,9,25,0,0,0,0,0">unknown</d>'
      // Dirty rows are dropped one by one instead of failing the whole file.
      + '<d p="-1,1,25,0,0,0,0,0">负时间</d><d p="1,1,25,0,0,0,0,0">bad\u0000text</d>'
      + '<d p="1,1,25">too few fields</d><d p="1,1,25,0,0,0,0,0"></d>'
      + '</i>',
    );

    expect(result.truncated).toBe(false);
    expect(result.totalEntries).toBe(1);
    expect(result.entries).toEqual([{
      id: "danmaku-0",
      start: 12.5,
      mode: "scroll",
      text: "八字段",
      color: "#ffffff",
      fontSize: 25,
    }]);
  });

  it("truncates oversized files by uniform sampling instead of keeping the first entries", () => {
    const count = 60_000;
    const rows = Array.from(
      { length: count },
      (_, index) => `<d p="${index / 2},1,25,16777215,0,0,0,0">entry-${index}</d>`,
    ).join("");
    const xml = `<i>${rows}</i>`;

    const result = parseDanmakuXml(xml, { random: deterministicRandom(7) });

    expect(result.truncated).toBe(true);
    expect(result.totalEntries).toBe(count);
    expect(result.entries).toHaveLength(DANMAKU_LIMITS.maxEntries);
    // Sampling must stay sorted by playback time and keep ids unique.
    const starts = result.entries.map((entry) => entry.start);
    expect(starts).toEqual([...starts].sort((left, right) => left - right));
    expect(new Set(result.entries.map((entry) => entry.id)).size).toBe(result.entries.length);
    // A sampled spread reaches both ends of the file; keeping the first N entries could not.
    expect(Math.min(...starts)).toBeLessThan((count / 2) * 0.1);
    expect(Math.max(...starts)).toBeGreaterThan((count / 2) * 0.9);
  });

  it("keeps a multi-megabyte file that exceeds the entry budget instead of rejecting it", () => {
    const rows = Array.from(
      { length: 120_000 },
      (_, index) => `<d p="${index % 86_400},1,25,16777215,0,0,0,0">filler-${index}</d>`,
    ).join("");
    const xml = `<i>${rows}</i>`;
    expect(exceedsUtf8ByteLimit(xml, 4 * 1024 * 1024)).toBe(true);
    // …but still inside the client's buffer/decode budget.
    expect(exceedsUtf8ByteLimit(xml, DANMAKU_LIMITS.maxBytes)).toBe(false);

    const result = parseDanmakuXml(xml, { random: deterministicRandom(3) });

    expect(result.truncated).toBe(true);
    expect(result.totalEntries).toBe(120_000);
    expect(result.entries).toHaveLength(DANMAKU_LIMITS.maxEntries);
  });

  it("still rejects a file beyond the server-side size limit", () => {
    const oversized = `<i>${" ".repeat(DANMAKU_LIMITS.maxBytes)}${NO_DANMAKU}</i>`;
    expect(() => parseDanmakuXml(oversized))
      .toThrowError(new DanmakuParseError("INPUT_TOO_LARGE", "弹幕文件过大"));
  });

  it("documents the client byte budget and the entry budget it samples against", () => {
    // The server serves up to 50 MiB, but the browser must not buffer, decode, and structured-clone
    // that on the main thread: the client stays at 16 MiB and samples beyond 50,000 entries.
    expect(DANMAKU_SERVER_MAX_BYTES).toBe(50 * 1024 * 1024);
    expect(DANMAKU_LIMITS.maxBytes).toBe(16 * 1024 * 1024);
    expect(DANMAKU_LIMITS.maxEntries).toBe(50_000);
  });

  it("keeps specific danmaku failure messages instead of degrading them", () => {
    // A Worker that dies reports “弹幕解析失败”; the user must see that, not a generic load error.
    expect(danmakuFailureMessage(new Error("弹幕解析失败"))).toBe("弹幕解析失败");
    expect(danmakuFailureMessage(new Error("弹幕文件过大"))).toBe("弹幕文件过大");
    expect(danmakuFailureMessage(new Error("弹幕内容编码无效"))).toBe("弹幕内容编码无效");
    // Unknown transport failures still collapse to the generic message (no internal detail leaks).
    expect(danmakuFailureMessage(new Error("danmaku-request-failed"))).toBe("弹幕加载失败");
    expect(danmakuFailureMessage(new Error("danmaku URL is not same-origin"))).toBe("弹幕加载失败");
  });

  it("decodes UTF-8 bytes with a fatal decoder", () => {
    expect(decodeDanmakuBytes(new TextEncoder().encode("弹幕").buffer)).toBe("弹幕");
    expect(() => decodeDanmakuBytes(new Uint8Array([0xff, 0xfe, 0x00]).buffer))
      .toThrowError(new Error("弹幕内容编码无效"));
  });

  it("reads a raw response as bytes and refuses one beyond the client budget", async () => {
    const small = await readDanmakuResponse(
      new Response("<i></i>"),
      new AbortController().signal,
    );
    expect(small).toBeInstanceOf(ArrayBuffer);
    expect(new TextDecoder().decode(small)).toBe("<i></i>");

    const declared = new Response("<i></i>", {
      headers: { "content-length": String(DANMAKU_LIMITS.maxBytes + 1) },
    });
    await expect(readDanmakuResponse(declared, new AbortController().signal))
      .rejects.toThrowError(new Error("弹幕文件过大"));

    // The streamed guard matters when a proxy omits content-length.
    const streamed = new Response("x".repeat(DANMAKU_LIMITS.maxBytes + 1));
    await expect(readDanmakuResponse(streamed, new AbortController().signal))
      .rejects.toThrowError(new Error("弹幕文件过大"));
  });

  it("decodes and parses bytes off the Worker path and rejects invalid UTF-8", async () => {
    const bytes = new TextEncoder().encode('<i><d p="1,1,25,16777215,0,0,0,0">字节</d></i>').buffer;
    const result = await parseDanmakuPayload(bytes);
    expect(result.entries).toEqual([{
      id: "danmaku-0",
      start: 1,
      mode: "scroll",
      text: "字节",
      color: "#ffffff",
      fontSize: 25,
    }]);

    await expect(parseDanmakuPayload(new Uint8Array([0xff, 0xfe, 0x00]).buffer))
      .rejects.toThrowError(new Error("弹幕内容编码无效"));
  });

  it("rejects invalid XML, unsafe declarations, and keeps DOCTYPE/ENTITY denied", () => {
    expect(() => parseDanmakuXml("<html>not danmaku</html>"))
      .toThrowError(new DanmakuParseError("INVALID_XML", "弹幕 XML 格式无效"));
    expect(() => parseDanmakuXml(
      '<i><i><d p="1,1,25,0,0,0,0,0">nested</d></i></i>',
    )).toThrowError(new DanmakuParseError("INVALID_XML", "弹幕 XML 格式无效"));
    expect(() => parseDanmakuXml(
      '<?xml?><i><d p="1,1,25,0,0,0,0,0">bad declaration</d></i>',
    )).toThrowError(new DanmakuParseError("INVALID_XML", "弹幕 XML 格式无效"));
    expect(() => parseDanmakuXml(
      '<!DOCTYPE i [<!ENTITY xxe SYSTEM "file:///etc/passwd">]><i><d p="1,1,25,0,0,0,0,0">&xxe;</d></i>',
    )).toThrowError(new DanmakuParseError("INVALID_XML", "弹幕 XML 格式无效"));
    expect(() => parseDanmakuXml(
      '<!ENTITY xxe "boom"><i><d p="1,1,25,0,0,0,0,0">x</d></i>',
    )).toThrowError(new DanmakuParseError("INVALID_XML", "弹幕 XML 格式无效"));
    expect(() => parseDanmakuXml(
      '<i><d p="1,1,25,0,0,0,0,0">unclosed</i>',
    )).toThrowError(new DanmakuParseError("INVALID_XML", "弹幕 XML 格式无效"));
    expect(() => parseDanmakuXml(
      '<i><d p="1,1,25,0,0,0,0,0">outer<d p="1,1,25,0,0,0,0,0">inner</d></d></i>',
    )).toThrowError(new DanmakuParseError("INVALID_XML", "弹幕 XML 格式无效"));
  });

  it("reports malformed XML even when the entry count is enormous", () => {
    expect(() => parseDanmakuXml(
      `<i>${"<d".repeat(5_001)}</i>`,
    )).toThrowError(new DanmakuParseError("INVALID_XML", "弹幕 XML 格式无效"));
  });

  it("keeps worker parsing failures scoped to the request generation", () => {
    expect(parseDanmakuWorkerRequest({
      type: "PARSE",
      requestId: 12,
      bytes: new TextEncoder().encode('<i><d p="1,1,25,16777215,0,0,0,0">worker</d></i>').buffer,
    })).toEqual({
      type: "PARSED",
      requestId: 12,
      entries: [{
        id: "danmaku-0",
        start: 1,
        mode: "scroll",
        text: "worker",
        color: "#ffffff",
        fontSize: 25,
      }],
      truncated: false,
      totalEntries: 1,
    });
    expect(parseDanmakuWorkerRequest({
      type: "PARSE",
      requestId: 13,
      bytes: new TextEncoder().encode("<bad />").buffer,
    })).toEqual({
      type: "FAILED",
      requestId: 13,
      message: "弹幕 XML 格式无效",
    });
    expect(parseDanmakuWorkerRequest({
      type: "PARSE",
      requestId: 14,
      bytes: new Uint8Array([0xff, 0xfe, 0x00]).buffer,
    })).toEqual({
      type: "FAILED",
      requestId: 14,
      message: "弹幕内容编码无效",
    });
  });

  it("reports truncation from the worker so the page can tell the user", () => {
    const rows = Array.from(
      { length: 60_000 },
      (_, index) => `<d p="${index / 2},1,25,16777215,0,0,0,0">row-${index}</d>`,
    ).join("");
    const response = parseDanmakuWorkerRequest({
      type: "PARSE",
      requestId: 21,
      bytes: new TextEncoder().encode(`<i>${rows}</i>`).buffer,
    });

    expect(response).toMatchObject({
      type: "PARSED",
      requestId: 21,
      truncated: true,
      totalEntries: 60_000,
    });
    expect(response.type === "PARSED" ? response.entries : []).toHaveLength(DANMAKU_LIMITS.maxEntries);
  });
});
