export type CaptionFormat = "srt" | "ass" | "ssa" | "vtt";

export type LuxCaptionCue = {
  id: string;
  start: number;
  end: number;
  text: string;
};

export type CaptionParseErrorCode =
  | "UNSUPPORTED_FORMAT"
  | "INPUT_TOO_LARGE"
  | "TOO_MANY_CUES"
  | "TEXT_TOO_LONG"
  | "CONTROL_CHARACTER"
  | "INVALID_CUE"
  | "INVALID_TIME";

export const CAPTION_LIMITS = {
  maxBytes: 1_048_576,
  maxCues: 5_000,
  maxTextLength: 500,
  maxDurationSeconds: 24 * 60 * 60,
} as const;

export class CaptionParseError extends Error {
  readonly code: CaptionParseErrorCode;

  constructor(code: CaptionParseErrorCode, message: string) {
    super(message);
    this.name = "CaptionParseError";
    this.code = code;
  }
}

export function parseCaptionText(input: string, format: CaptionFormat): LuxCaptionCue[] {
  if (!isCaptionFormat(format)) {
    throw new CaptionParseError("UNSUPPORTED_FORMAT", "字幕格式不受支持");
  }
  assertInputBounds(input);
  const normalized = input.replace(/^\uFEFF/, "").replace(/\r\n?/g, "\n");
  const cues = format === "srt"
    ? parseSrt(normalized)
    : format === "vtt"
      ? parseVtt(normalized)
      : parseAss(normalized);
  return cues
    .sort((left, right) => left.start - right.start || left.end - right.end)
    .map((cue, index) => ({ ...cue, id: `cue-${index}` }));
}

function parseSrt(input: string): LuxCaptionCue[] {
  const cues: LuxCaptionCue[] = [];
  for (const block of nonEmptyBlocks(input)) {
    const lines = block.split("\n");
    const timeLineIndex = lines.findIndex((line) => line.includes("-->"));
    if (timeLineIndex < 0) throw invalidCue();
    const [startValue, endValue] = splitTimeLine(lines[timeLineIndex]);
    const { start, end } = parseRange(startValue, endValue, "srt");
    const text = lines.slice(timeLineIndex + 1).join("\n").trim();
    appendCue(cues, makeCue(start, end, text));
  }
  return cues;
}

function parseVtt(input: string): LuxCaptionCue[] {
  const cues: LuxCaptionCue[] = [];
  for (const block of nonEmptyBlocks(input)) {
    const lines = block.split("\n");
    if (/^(WEBVTT|NOTE|STYLE|REGION)(?:\s|$)/i.test(lines[0])) continue;
    const timeLineIndex = lines.findIndex((line) => line.includes("-->"));
    if (timeLineIndex < 0) continue;
    const [startValue, endValue] = splitTimeLine(lines[timeLineIndex]);
    const { start, end } = parseRange(startValue, endValue.split(/\s+/, 1)[0], "vtt");
    const text = lines.slice(timeLineIndex + 1).join("\n").trim();
    appendCue(cues, makeCue(start, end, text));
  }
  return cues;
}

function parseAss(input: string): LuxCaptionCue[] {
  const cues: LuxCaptionCue[] = [];
  for (const line of input.split("\n")) {
    if (!/^Dialogue\s*:/i.test(line)) continue;
    const fields = line.slice(line.indexOf(":") + 1).trim().split(",");
    if (fields.length < 10) throw invalidCue();
    const { start, end } = parseRange(fields[1], fields[2], "ass");
    const text = fields.slice(9).join(",")
      .replace(/\{[^{}\n]{0,256}\}/g, "")
      .replace(/\\N|\\n/g, "\n")
      .replace(/\\h/g, " ")
      .trim();
    appendCue(cues, makeCue(start, end, text));
  }
  return cues;
}

function nonEmptyBlocks(input: string) {
  return input
    .split(/\n{2,}/)
    .map((block) => block.trim())
    .filter((block) => block.length > 0);
}

function splitTimeLine(line: string): [string, string] {
  const parts = line.split("-->");
  if (parts.length !== 2) throw invalidTime("字幕时间格式无效");
  return [parts[0].trim(), parts[1].trim()];
}

function parseRange(startValue: string, endValue: string, format: "srt" | "ass" | "vtt") {
  const start = parseTimestamp(startValue, format);
  const end = parseTimestamp(endValue, format);
  if (start < 0 || end < 0 || end <= start) {
    throw new CaptionParseError("INVALID_TIME", "字幕时间范围无效");
  }
  if (end > CAPTION_LIMITS.maxDurationSeconds) {
    throw new CaptionParseError("INVALID_TIME", "字幕时间超出范围");
  }
  return { start, end };
}

function parseTimestamp(value: string, format: "srt" | "ass" | "vtt") {
  const match = format === "ass"
    ? /^(\d+):(\d{1,2}):(\d{1,2})\.(\d{2})$/.exec(value)
    : format === "vtt"
      ? /^(?:(\d+):)?(\d{1,2}):(\d{2})\.(\d{3})$/.exec(value)
      : /^(\d+):(\d{2}):(\d{2})[,.](\d{3})$/.exec(value);
  if (!match) throw invalidTime("字幕时间格式无效");
  const hours = Number(match[1] ?? 0);
  const minutes = Number(match[2]);
  const seconds = Number(match[3]);
  const fraction = Number(match[4]) / (format === "ass" ? 100 : 1_000);
  if (minutes > 59 || seconds > 59 || ![hours, minutes, seconds, fraction].every(Number.isFinite)) {
    throw invalidTime("字幕时间格式无效");
  }
  return hours * 3_600 + minutes * 60 + seconds + fraction;
}

function makeCue(start: number, end: number, text: string): LuxCaptionCue {
  if (!text) throw invalidCue();
  assertSafeText(text);
  if (text.length > CAPTION_LIMITS.maxTextLength) {
    throw new CaptionParseError("TEXT_TOO_LONG", "字幕文本过长");
  }
  return { id: "", start, end, text };
}

function appendCue(cues: LuxCaptionCue[], cue: LuxCaptionCue) {
  if (cues.length >= CAPTION_LIMITS.maxCues) {
    throw new CaptionParseError("TOO_MANY_CUES", "字幕条目过多");
  }
  cues.push(cue);
}

function assertInputBounds(input: string) {
  if (typeof input !== "string") {
    throw new CaptionParseError("INVALID_CUE", "字幕内容无效");
  }
  const bytes = new TextEncoder().encode(input).byteLength;
  if (bytes > CAPTION_LIMITS.maxBytes) {
    throw new CaptionParseError("INPUT_TOO_LARGE", "字幕文件过大");
  }
  assertSafeText(input);
}

function assertSafeText(text: string) {
  if (/[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F]/u.test(text)) {
    throw new CaptionParseError("CONTROL_CHARACTER", "字幕包含不支持的控制字符");
  }
}

function invalidCue() {
  return new CaptionParseError("INVALID_CUE", "字幕条目格式无效");
}

function invalidTime(message: string) {
  return new CaptionParseError("INVALID_TIME", message);
}

function isCaptionFormat(value: string): value is CaptionFormat {
  return value === "srt" || value === "ass" || value === "ssa" || value === "vtt";
}
