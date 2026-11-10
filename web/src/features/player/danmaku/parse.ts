/**
 * Pure Bilibili danmaku XML parsing.
 *
 * Nothing here touches the DOM: the Worker (`danmaku-worker.ts`) and the fallback path in
 * `load.ts` both call these functions, so the on-screen renderer never parses XML on the main
 * thread when Workers are available.
 */

export type DanmakuMode = "scroll" | "top" | "bottom";

export type LuxDanmakuEntry = {
  id: string;
  start: number;
  mode: DanmakuMode;
  text: string;
  color: string;
  fontSize: number;
};

/**
 * Server-side cap (`MAX_DANMAKU_XML_BYTES` in `application::danmaku`). The browser deliberately
 * stays below it — see `DANMAKU_LIMITS.maxBytes` — but the number is recorded here because the
 * client must never assume it can load everything the server can serve.
 */
export const DANMAKU_SERVER_MAX_BYTES = 50 * 1024 * 1024;

export const DANMAKU_LIMITS = {
  /**
   * Client buffer/decode budget. The server serves up to 50 MiB, but the browser would otherwise
   * buffer, decode, and structured-clone that much text on a low-memory device; 16 MiB is the
   * largest payload we accept, and anything above it reports “弹幕文件过大”.
   */
  maxBytes: 16 * 1024 * 1024,
  /**
   * Parse/render budget. Larger files are kept but uniformly sampled down to this many entries
   * (reservoir sampling) instead of being rejected or cut to the first N rows.
   */
  maxEntries: 50_000,
  maxTextLength: 200,
  maxTimeSeconds: 24 * 60 * 60,
  minFontSize: 12,
  maxFontSize: 64,
  maxVisible: 80,
} as const;

export type DanmakuParseErrorCode = "INVALID_XML" | "INPUT_TOO_LARGE";

export class DanmakuParseError extends Error {
  readonly code: DanmakuParseErrorCode;

  constructor(code: DanmakuParseErrorCode, message: string) {
    super(message);
    this.name = "DanmakuParseError";
    this.code = code;
  }
}

export type DanmakuParseResult = {
  entries: LuxDanmakuEntry[];
  /** Set when the file carried more usable entries than the parse budget. */
  truncated: boolean;
  /** Number of usable entries found in the file before sampling. */
  totalEntries: number;
};

export type DanmakuParseOptions = {
  /** Overrides `DANMAKU_LIMITS.maxBytes` (tests, or a tighter mobile budget). */
  maxBytes?: number;
  /** Overrides `DANMAKU_LIMITS.maxEntries` (tests, or a tighter mobile budget). */
  maxEntries?: number;
  /** Injectable source of randomness so sampling stays reproducible in tests. */
  random?: () => number;
};

const XML_DECLARATION_PATTERN = /^<\?xml\s+version\s*=\s*(?:"1\.[01]"|'1\.[01]')(?:\s+encoding\s*=\s*(?:"[A-Za-z][A-Za-z0-9._-]*"|'[A-Za-z][A-Za-z0-9._-]*'))?(?:\s+standalone\s*=\s*(?:"(?:yes|no)"|'(?:yes|no)'))?\s*\?>$/i;
const DENIED_DECLARATION_PATTERN = /<!(?:DOCTYPE|ENTITY)\b/i;
const CONTROL_CHARACTER_PATTERN = /[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F]/u;

/**
 * Decodes raw danmaku bytes with a fatal UTF-8 decoder.
 *
 * Runs inside the Worker (the bytes are transferred there, so the main thread never decodes or
 * copies the payload); the non-Worker fallback calls the same helper.
 */
export function decodeDanmakuBytes(bytes: ArrayBuffer | ArrayBufferView): string {
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    throw new Error("弹幕内容编码无效");
  }
}

export function exceedsUtf8ByteLimit(input: string, limit: number): boolean {
  if (input.length > limit) return true;

  let bytes = 0;
  for (let index = 0; index < input.length; index += 1) {
    const codePoint = input.codePointAt(index) ?? 0;
    if (codePoint <= 0x7f) {
      bytes += 1;
    } else if (codePoint <= 0x7ff) {
      bytes += 2;
    } else if (codePoint <= 0xffff) {
      bytes += 3;
    } else {
      bytes += 4;
      index += 1;
    }
    if (bytes > limit) return true;
  }
  return false;
}

/**
 * Parses Bilibili XML into renderable entries.
 *
 * Only the first four `p` fields are read (time, mode, font size, colour); the remaining fields
 * Bilibili writes (`timestamp`, pool, sender hash, row id) are ignored. Advanced (mode 7) and code
 * (mode 8) danmaku, unsupported modes, and dirty rows are skipped one by one: a bad row must never
 * fail the whole file.
 */
export function parseDanmakuXml(
  input: string,
  options: DanmakuParseOptions = {},
): DanmakuParseResult {
  if (typeof input !== "string") {
    throw invalidXml();
  }
  const maxBytes = options.maxBytes ?? DANMAKU_LIMITS.maxBytes;
  if (exceedsUtf8ByteLimit(input, maxBytes)) {
    throw new DanmakuParseError("INPUT_TOO_LARGE", "弹幕文件过大");
  }
  if (DENIED_DECLARATION_PATTERN.test(input)) {
    throw invalidXml();
  }

  const xmlDocument = withoutXmlDeclaration(input);
  const root = /^\s*<i\b[^>]*>([\s\S]*)<\/i>\s*$/i.exec(xmlDocument);
  if (!root || /<\/i\s*>[\s\S]*<i\b/i.test(input)) {
    throw invalidXml();
  }
  const body = root[1] ?? "";
  if (/<i\b/i.test(body)) {
    throw invalidXml();
  }

  const maxEntries = Math.max(1, Math.floor(options.maxEntries ?? DANMAKU_LIMITS.maxEntries));
  const random = options.random ?? Math.random;
  // Reservoir sampling keeps memory bounded by `maxEntries` while still covering the whole file,
  // so a huge file shows danmaku from the end as well as the beginning.
  const sample: Array<Omit<LuxDanmakuEntry, "id">> = [];
  let totalEntries = 0;
  let openingCount = 0;
  let closingCount = 0;
  let malformed = false;

  const tagPattern = /<d\b([^>]*)>([\s\S]*?)<\/d\s*>|<d\b|<\/d\s*>/gi;
  for (const match of body.matchAll(tagPattern)) {
    const tag = match[0];
    const attributes = match[1];
    const rawText = match[2];
    if (attributes !== undefined && rawText !== undefined) {
      openingCount += 1;
      closingCount += 1;
      if (
        (attributes.includes("<") && /<d\b/i.test(attributes))
        || (rawText.includes("<") && /<d\b/i.test(rawText))
      ) {
        malformed = true;
      }
      const entry = parseEntry(attributes, rawText);
      if (!entry) continue;
      const index = totalEntries;
      totalEntries += 1;
      if (index < maxEntries) {
        sample.push(entry);
      } else {
        const candidate = Math.floor(random() * (index + 1));
        if (candidate < maxEntries) sample[candidate] = entry;
      }
      continue;
    }
    if (tag.startsWith("</")) {
      closingCount += 1;
      continue;
    }

    openingCount += 1;
  }
  if (malformed || openingCount !== closingCount) {
    throw invalidXml();
  }

  sample.sort((left, right) => left.start - right.start);
  const entries = sample.map((entry, index) => ({ id: `danmaku-${index}`, ...entry }));
  return { entries, truncated: totalEntries > maxEntries, totalEntries };
}

/** Convenience wrapper for callers that only need the entries. */
export function parseBilibiliDanmaku(input: string): LuxDanmakuEntry[] {
  return parseDanmakuXml(input).entries;
}

/**
 * Applies the shared entry rules (`docs/LUX-DEVELOPMENT.md` §14.5).
 *
 * A dirty row returns `null` so the caller drops it. The font size is the one field the rules
 * only require to be finite: the server normalizer clamps it to 12–64, so the renderer does the
 * same instead of hiding entries the server accepts.
 */
function parseEntry(attributes: string, rawText: string): Omit<LuxDanmakuEntry, "id"> | null {
  if (rawText.includes("<")) return null;
  const values = /(?:^|[\s])p\s*=\s*(["'])(.*?)\1/i.exec(attributes)?.[2]?.split(",") ?? [];
  // Bilibili writes eight `p` fields; four are enough, any extras are ignored on purpose.
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
    || !Number.isInteger(colorValue)
    || colorValue < 0
    || colorValue > 0xffffff
    || !text
    // Count Unicode code points, not UTF-16 units: the server applies the same rule
    // (MAX_DANMAKU_TEXT_CHARS), and the two sides must agree on shared validity vectors.
    || [...text].length > DANMAKU_LIMITS.maxTextLength
    || CONTROL_CHARACTER_PATTERN.test(text)
  ) {
    return null;
  }
  return {
    start,
    mode,
    text,
    color: `#${colorValue.toString(16).padStart(6, "0")}`,
    // The size only has to be a finite number to be usable; out-of-range sizes are clamped
    // instead of dropped so the server-side normalization job and the renderer agree.
    fontSize: Math.min(
      DANMAKU_LIMITS.maxFontSize,
      Math.max(DANMAKU_LIMITS.minFontSize, fontSize),
    ),
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
  // Mode 7 (advanced/positioned) and mode 8 (code) are not drawn by this renderer yet.
  return null;
}

function withoutXmlDeclaration(input: string) {
  const xmlDocument = input.trimStart();
  if (!/^<\?xml\b/i.test(xmlDocument)) return xmlDocument;
  const end = xmlDocument.indexOf("?>");
  if (end < 0) throw invalidXml();
  const declaration = xmlDocument.slice(0, end + 2);
  if (!XML_DECLARATION_PATTERN.test(declaration)) {
    throw invalidXml();
  }
  return xmlDocument.slice(end + 2);
}

function invalidXml() {
  return new DanmakuParseError("INVALID_XML", "弹幕 XML 格式无效");
}
