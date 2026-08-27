export type DanmakuMode = "scroll" | "top" | "bottom";

export type LuxDanmakuEntry = {
  id: string;
  start: number;
  mode: DanmakuMode;
  text: string;
  color: string;
  fontSize: number;
};

export type ScheduledDanmaku = LuxDanmakuEntry & {
  lane: number;
  duration: number;
  estimatedWidth: number;
  lineHeight: number;
};

export type DanmakuPlacement = ScheduledDanmaku & {
  x: number;
  y: number;
  progress: number;
};

export const DANMAKU_LIMITS = {
  maxBytes: 4 * 1024 * 1024,
  maxEntries: 5_000,
  maxTextLength: 200,
  maxTimeSeconds: 24 * 60 * 60,
  minFontSize: 12,
  maxFontSize: 64,
  maxVisible: 80,
} as const;

const SCROLL_LINE_HEIGHT = 32;
const FIXED_LINE_HEIGHT = 34;

function laneGeometry(height: number) {
  const fixedLaneCount = Math.max(2, Math.min(4, Math.floor((height * 0.16) / FIXED_LINE_HEIGHT)));
  const fixedBandHeight = fixedLaneCount * FIXED_LINE_HEIGHT + 8;
  const availableScrollHeight = Math.max(SCROLL_LINE_HEIGHT * 4, height - fixedBandHeight * 2);
  const scrollLaneCount = Math.max(4, Math.min(18, Math.floor((availableScrollHeight * 0.64) / SCROLL_LINE_HEIGHT)));
  return { fixedLaneCount, fixedBandHeight, scrollLaneCount };
}

export type DanmakuParseErrorCode =
  | "INVALID_XML"
  | "INPUT_TOO_LARGE"
  | "TOO_MANY_ENTRIES";

export class DanmakuParseError extends Error {
  readonly code: DanmakuParseErrorCode;

  constructor(code: DanmakuParseErrorCode, message: string) {
    super(message);
    this.name = "DanmakuParseError";
    this.code = code;
  }
}

export function parseBilibiliDanmaku(input: string): LuxDanmakuEntry[] {
  if (typeof input !== "string") {
    throw invalidXml();
  }
  if (new TextEncoder().encode(input).byteLength > DANMAKU_LIMITS.maxBytes) {
    throw new DanmakuParseError("INPUT_TOO_LARGE", "弹幕文件过大");
  }
  if (/<!(?:DOCTYPE|ENTITY)\b/i.test(input)) {
    throw invalidXml();
  }

  const root = /^\s*<i\b[^>]*>([\s\S]*)<\/i>\s*$/i.exec(input);
  if (!root || /<\/i\s*>[\s\S]*<i\b/i.test(input)) {
    throw invalidXml();
  }
  const body = root[1] ?? "";
  if (/<i\b/i.test(body)) {
    throw invalidXml();
  }
  const openingCount = (body.match(/<d\b/gi) ?? []).length;
  const closingCount = (body.match(/<\/d\s*>/gi) ?? []).length;
  if (openingCount !== closingCount) {
    throw invalidXml();
  }
  if (openingCount > DANMAKU_LIMITS.maxEntries) {
    throw new DanmakuParseError("TOO_MANY_ENTRIES", "弹幕条目过多");
  }

  const entries: LuxDanmakuEntry[] = [];
  const entryPattern = /<d\b([^>]*)>([\s\S]*?)<\/d\s*>/gi;
  for (const match of body.matchAll(entryPattern)) {
    const entry = parseEntry(match[1] ?? "", match[2] ?? "");
    if (entry) entries.push({ id: "", ...entry });
  }
  return entries
    .sort((left, right) => left.start - right.start)
    .map((entry, index) => ({ ...entry, id: `danmaku-${index}` }));
}

function parseEntry(attributes: string, rawText: string): Omit<LuxDanmakuEntry, "id"> | null {
  if (rawText.includes("<")) return null;
  const values = /\bp\s*=\s*(["'])(.*?)\1/i.exec(attributes)?.[2]?.split(",") ?? [];
  if (values.length < 4) return null;
  const start = Number(values[0]);
  const mode = normalizeMode(Number(values[1]));
  const fontSize = Number(values[2]);
  const colorValue = Number(values[3]);
  const text = decodeXmlText(rawText).replace(/\r\n?/g, "\n").trim();
  if (
    !mode
    || !Number.isFinite(start)
    || start < 0
    || start > DANMAKU_LIMITS.maxTimeSeconds
    || !Number.isFinite(fontSize)
    || fontSize < DANMAKU_LIMITS.minFontSize
    || fontSize > DANMAKU_LIMITS.maxFontSize
    || !Number.isInteger(colorValue)
    || colorValue < 0
    || colorValue > 0xffffff
    || !text
    || text.length > DANMAKU_LIMITS.maxTextLength
    || /[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F]/u.test(text)
  ) {
    return null;
  }
  return {
    start,
    mode,
    text,
    color: `#${colorValue.toString(16).padStart(6, "0")}`,
    fontSize,
  };
}

function decodeXmlText(value: string) {
  return value.replace(
    /&(#x[\da-f]+|#\d+|lt|gt|amp|quot|apos);/gi,
    (entity, name: string) => {
      const normalized = name.toLowerCase();
      if (normalized === "lt") return "<";
      if (normalized === "gt") return ">";
      if (normalized === "amp") return "&";
      if (normalized === "quot") return '"';
      if (normalized === "apos") return "'";
      const codePoint = normalized.startsWith("#x")
        ? Number.parseInt(normalized.slice(2), 16)
        : Number.parseInt(normalized.slice(1), 10);
      return Number.isSafeInteger(codePoint) && codePoint >= 0 && codePoint <= 0x10ffff
        ? String.fromCodePoint(codePoint)
        : entity;
    },
  );
}

function normalizeMode(value: number): DanmakuMode | null {
  if (value === 4) return "bottom";
  if (value === 5) return "top";
  if ([1, 2, 3, 6].includes(value)) return "scroll";
  return null;
}

function invalidXml() {
  return new DanmakuParseError("INVALID_XML", "弹幕 XML 格式无效");
}

export function assignDanmakuLanes(
  entries: readonly LuxDanmakuEntry[],
  viewport: { width: number; height: number },
): ScheduledDanmaku[] {
  const width = Math.max(1, viewport.width);
  const height = Math.max(1, viewport.height);
  const { fixedLaneCount, scrollLaneCount } = laneGeometry(height);
  const lastByMode: Record<DanmakuMode, Array<ScheduledDanmaku | undefined>> = {
    scroll: Array.from({ length: scrollLaneCount }),
    top: Array.from({ length: fixedLaneCount }),
    bottom: Array.from({ length: fixedLaneCount }),
  };
  const result: ScheduledDanmaku[] = [];

  for (const entry of [...entries].sort((left, right) => left.start - right.start)) {
    const lineHeight = entry.mode === "scroll" ? SCROLL_LINE_HEIGHT : FIXED_LINE_HEIGHT;
    const duration = entry.mode === "scroll" ? 8 : 4;
    const estimatedWidth = Math.min(width * 0.9, Math.max(48, entry.text.length * entry.fontSize * 0.95 + 16));
    const lanes = lastByMode[entry.mode];
    const availableLane = lanes.findIndex((last) => (
      !last
      || entry.mode === "scroll"
        ? !last || entry.start >= last.start + last.duration * 0.42
        : entry.start >= last.start + last.duration
    ));
    const lane = availableLane >= 0
      ? availableLane
      : lanes.reduce((oldest, last, index) => (
        !last || !lanes[oldest] || last.start < lanes[oldest]!.start ? index : oldest
      ), 0);
    const scheduled = { ...entry, lane, duration, estimatedWidth, lineHeight };
    lanes[lane] = scheduled;
    result.push(scheduled);
  }

  return result;
}

export function activeDanmaku(
  entries: readonly ScheduledDanmaku[],
  time: number,
  viewport: { width: number; height: number } = { width: 1280, height: 720 },
  _playbackRate = 1,
): DanmakuPlacement[] {
  if (!Number.isFinite(time) || time < 0) return [];
  const width = Math.max(1, viewport.width);
  const height = Math.max(1, viewport.height);
  const { fixedBandHeight } = laneGeometry(height);
  const candidates = entries
    .filter((entry) => time >= entry.start && time < entry.start + entry.duration)
    .sort((left, right) => right.start - left.start);
  const selected: DanmakuPlacement[] = [];
  for (const entry of candidates) {
    if (selected.length >= DANMAKU_LIMITS.maxVisible) break;
    const sameLane = selected.some((other) => (
      other.mode === entry.mode
      && other.lane === entry.lane
      && (entry.mode !== "scroll" || entry.start < other.start + other.duration * 0.42)
    ));
    if (sameLane) continue;
    const elapsed = time - entry.start;
    const progress = Math.max(0, Math.min(1, elapsed / entry.duration));
    const x = entry.mode === "scroll"
      ? width - progress * (width + entry.estimatedWidth)
      : width / 2;
    const y = entry.mode === "bottom"
      ? height - (entry.lane + 1) * FIXED_LINE_HEIGHT - 8
      : entry.mode === "top"
        ? entry.lane * FIXED_LINE_HEIGHT + 8
        : fixedBandHeight + entry.lane * SCROLL_LINE_HEIGHT + 8;
    selected.push({ ...entry, x, y, progress });
  }
  return selected.sort((left, right) => left.start - right.start);
}
