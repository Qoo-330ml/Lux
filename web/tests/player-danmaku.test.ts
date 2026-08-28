import { describe, expect, it } from "vitest";
import {
  DanmakuParseError,
  activeDanmaku,
  assignDanmakuLanes,
  exceedsUtf8ByteLimit,
  parseBilibiliDanmaku,
} from "../src/features/player/danmaku";
import { parseDanmakuWorkerRequest } from "../src/features/player/danmaku-worker";

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

  it("rejects invalid XML and discards unsafe or unsupported entries", () => {
    expect(() => parseBilibiliDanmaku("<html>not danmaku</html>"))
      .toThrowError(new DanmakuParseError("INVALID_XML", "弹幕 XML 格式无效"));
    expect(() => parseBilibiliDanmaku(
      '<i><i><d p="1,1,25,0,0,0,0,0">nested</d></i></i>',
    )).toThrowError(new DanmakuParseError("INVALID_XML", "弹幕 XML 格式无效"));
    expect(parseBilibiliDanmaku(
      '<i><d p="1,7,25,0,0,0,0,0">高级模式</d><d p="-1,1,25,0,0,0,0,0">负时间</d><d p="1,1,25,0,0,0,0,0">safe\u0000text</d><d p="1,1,25,0,0,0,0,0"></d></i>',
    )).toEqual([]);
    expect(() => parseBilibiliDanmaku(
      '<i><d p="1,1,25,0,0,0,0,0">unclosed</i>',
    )).toThrowError(new DanmakuParseError("INVALID_XML", "弹幕 XML 格式无效"));
    expect(() => parseBilibiliDanmaku(
      '<i><d p="1,1,25,0,0,0,0,0">outer<d p="1,1,25,0,0,0,0,0">inner</d></d></i>',
    )).toThrowError(new DanmakuParseError("INVALID_XML", "弹幕 XML 格式无效"));
  });

  it("rejects more raw entries than the parser limit", () => {
    const entry = '<d p="1,1,25,0,0,0,0,0">entry</d>';
    expect(() => parseBilibiliDanmaku(
      `<i>${entry.repeat(5_001)}</i>`,
    )).toThrowError(new DanmakuParseError("TOO_MANY_ENTRIES", "弹幕条目过多"));
  });

  it("keeps worker parsing failures scoped to the request generation", () => {
    expect(parseDanmakuWorkerRequest({
      type: "PARSE",
      requestId: 12,
      xml: '<i><d p="1,1,25,16777215,0,0,0,0">worker</d></i>',
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
    });
    expect(parseDanmakuWorkerRequest({
      type: "PARSE",
      requestId: 13,
      xml: "<bad />",
    })).toEqual({
      type: "FAILED",
      requestId: 13,
      message: "弹幕 XML 格式无效",
    });
  });
});

describe("LuxPlayer danmaku lane scheduler", () => {
  it("assigns scrolling, top, and bottom danmaku to independent non-overlapping lanes", () => {
    const entries = parseBilibiliDanmaku(
      '<i><d p="0,1,25,0,0,0,0,0">first</d><d p="0.1,1,25,0,0,0,0,0">second</d><d p="1,5,25,0,0,0,0,0">top</d><d p="2,4,25,0,0,0,0,0">bottom</d></i>',
    );
    expect(assignDanmakuLanes(entries, { width: 900, height: 450 })).toEqual([
      expect.objectContaining({ id: "danmaku-0", lane: 0, duration: 8 }),
      expect.objectContaining({ id: "danmaku-1", lane: 1, duration: 8 }),
      expect.objectContaining({ id: "danmaku-2", lane: 0, duration: 4 }),
      expect.objectContaining({ id: "danmaku-3", lane: 0, duration: 4 }),
    ]);
    const safeZoneEntries = [entries[0]!, entries[2]!, entries[3]!];
    const placements = activeDanmaku(
      assignDanmakuLanes(safeZoneEntries, { width: 390, height: 590 }),
      2.5,
      { width: 390, height: 590 },
    );
    expect(new Set(placements.map((placement) => placement.y)).size).toBe(3);
  });

  it("keeps scheduling bounded and activates only entries at the playback time", () => {
    const entries = Array.from({ length: 1_000 }, (_, index) => ({
      id: `danmaku-${index}`,
      start: index / 10,
      mode: "scroll" as const,
      text: "弹幕",
      color: "#ffffff",
      fontSize: 25,
    }));
    const scheduled = assignDanmakuLanes(entries, { width: 320, height: 180 });
    expect(scheduled).toHaveLength(1_000);
    expect(scheduled.every((entry) => entry.lane >= 0 && entry.lane < 4)).toBe(true);
    expect(activeDanmaku(scheduled, 10)).toEqual(
      expect.arrayContaining([expect.objectContaining({ id: "danmaku-100" })]),
    );
    expect(activeDanmaku(scheduled, 10).length).toBeLessThanOrEqual(80);
  });
});
