import { describe, expect, it } from "vitest";
import { parseMatroskaSubtitleSample } from "../src/features/player/matroska-subtitles";

const bytes = (value: string) => new TextEncoder().encode(value);

describe("Matroska text subtitle parser", () => {
  it("parses UTF-8 cues without HTML interpretation", () => {
    const cue = parseMatroskaSubtitleSample(bytes("<b>字幕</b>"), { codecId: "S_TEXT/UTF8" }, 1000, 2000);
    expect(cue).toMatchObject({ start: 1, end: 3, text: "<b>字幕</b>" });
  });

  it("parses ASS styles, safe override tags, and PlayRes positions", () => {
    const header = bytes(`[Script Info]\nPlayResX: 1920\nPlayResY: 1080\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,Arial,40,&H00FFFFFF,&H00FFFFFF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,0,2,20,20,30,1`);
    const cue = parseMatroskaSubtitleSample(bytes("Dialogue: 0,0:00:01.00,0:00:03.00,Default,,,,,,{\\b1\\c&H0000FF\\pos(960,900)}红{\\i1}蓝{\\r}默认"), { codecId: "S_TEXT/ASS", codecPrivate: header }, 1000, 2000);
    expect(cue).toMatchObject({ alignment: 2, position: { x: 50, y: expect.closeTo(83.333, 2) } });
    expect(cue?.runs?.some((run) => run.bold)).toBe(true);
    expect(cue?.runs?.some((run) => run.italic)).toBe(true);
  });

  it("drops ASS drawing and rejects invalid UTF-8", () => {
    expect(parseMatroskaSubtitleSample(bytes("Dialogue: 0,0:00:01.00,0:00:02.00,Default,,,,,,{\\p1}m 0 0 l 1 1"), { codecId: "S_TEXT/SSA" }, 1000, 1000)).toBeNull();
    expect(() => parseMatroskaSubtitleSample(new Uint8Array([0xff]), { codecId: "S_TEXT/UTF8" }, 0, 1000)).toThrow("编码无效");
  });
});
