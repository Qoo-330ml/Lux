import { describe, expect, it } from "vitest";
import {
  CaptionParseError,
  parseCaptionText,
} from "../src/features/player/caption-parser";
import { parseCaptionWorkerRequest } from "../src/features/player/caption-parser-worker";

describe("LuxPlayer safe caption parser", () => {
  it("normalizes ordered SRT, ASS, SSA, and VTT cues", () => {
    expect(parseCaptionText(
      "2\n00:00:03,000 --> 00:00:04,500\n<b>later</b>\n\n1\n00:00:01,000 --> 00:00:02,000\nfirst",
      "srt",
    )).toEqual([
      { id: "cue-0", start: 1, end: 2, text: "first" },
      { id: "cue-1", start: 3, end: 4.5, text: "<b>later</b>" },
    ]);

    expect(parseCaptionText(
      "[Events]\nDialogue: Marked=0,0:00:02.00,0:00:04.50,Default,,,,,,Hello\\Nworld",
      "ass",
    )).toEqual([{ id: "cue-0", start: 2, end: 4.5, text: "Hello\nworld" }]);

    expect(parseCaptionText(
      "WEBVTT\n\nintro\n00:00.000 --> 00:01.250 align:start\n<em>hello</em>",
      "vtt",
    )).toEqual([{ id: "cue-0", start: 0, end: 1.25, text: "<em>hello</em>" }]);
  });

  it("rejects malformed, unsafe, and oversized input with stable errors", () => {
    expect(() => parseCaptionText("1\n00:00:03,000 --> 00:00:02,000\nwrong", "srt"))
      .toThrowError(new CaptionParseError("INVALID_TIME", "字幕时间范围无效"));
    expect(() => parseCaptionText("Dialogue: 0,0:00:01.00,not-a-time,Default,,,,,,bad", "ssa"))
      .toThrowError(new CaptionParseError("INVALID_TIME", "字幕时间格式无效"));
    expect(() => parseCaptionText("1\n00:00:00,000 --> 00:00:01,000\nzero\u0000byte", "srt"))
      .toThrowError(new CaptionParseError("CONTROL_CHARACTER", "字幕包含不支持的控制字符"));
    expect(() => parseCaptionText("x".repeat(1_048_577), "vtt"))
      .toThrowError(new CaptionParseError("INPUT_TOO_LARGE", "字幕文件过大"));
    const tooManyCues = Array.from(
      { length: 5_001 },
      (_, index) => `${index + 1}\n00:00:00,000 --> 00:00:01,000\ncue`,
    ).join("\n\n");
    expect(() => parseCaptionText(tooManyCues, "srt"))
      .toThrowError(new CaptionParseError("TOO_MANY_CUES", "字幕条目过多"));
  });

  it("keeps worker parsing results and failures inside a request generation", () => {
    expect(parseCaptionWorkerRequest({
      type: "PARSE",
      requestId: 7,
      format: "vtt",
      text: "WEBVTT\n\n00:00.000 --> 00:01.000\nworker cue",
    })).toEqual({
      type: "PARSED",
      requestId: 7,
      cues: [{ id: "cue-0", start: 0, end: 1, text: "worker cue" }],
    });
    expect(parseCaptionWorkerRequest({
      type: "PARSE",
      requestId: 8,
      format: "srt",
      text: "invalid",
    })).toEqual({
      type: "FAILED",
      requestId: 8,
      message: "字幕条目格式无效",
    });
  });
});
