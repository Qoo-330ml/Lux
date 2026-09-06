import type { LuxCaptionCue } from "./caption-parser";

export type MatroskaSubtitleTrack = {
  codecId: string;
  codecPrivate?: Uint8Array;
};

const MAX_TEXT = 64 * 1024;
const MAX_RUNS = 256;

export function parseMatroskaSubtitleSample(
  data: Uint8Array,
  track: MatroskaSubtitleTrack,
  startMs: number,
  durationMs: number,
): LuxCaptionCue | null {
  if (!Number.isFinite(startMs) || !Number.isFinite(durationMs) || durationMs <= 0) return null;
  let text: string;
  try {
    text = new TextDecoder("utf-8", { fatal: true }).decode(data);
  } catch {
    throw new Error("MKV 字幕编码无效");
  }
  if (text.length === 0 || text.length > MAX_TEXT || /[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F]/u.test(text)) {
    throw new Error("MKV 字幕文本无效");
  }
  const codec = track.codecId.trim().toUpperCase();
  if (codec === "S_TEXT/UTF8") {
    const value = text.trim();
    return value ? { id: "", start: startMs / 1000, end: (startMs + durationMs) / 1000, text: value } : null;
  }
  if (codec !== "S_TEXT/ASS" && codec !== "S_TEXT/SSA") return null;
  const header = parseAssHeader(track.codecPrivate);
  const dialogue = parseDialogue(text);
  if (!dialogue) return null;
  if (dialogue.drawing) return null;
  const styled = parseStyledText(dialogue.text, header, dialogue.style);
  if (!styled.text) return null;
  return {
    id: "",
    start: startMs / 1000,
    end: (startMs + durationMs) / 1000,
    text: styled.text,
    layer: dialogue.layer,
    alignment: styled.alignment,
    position: styled.position,
    style: styled.style,
    runs: styled.runs,
  };
}

type AssHeader = {
  playResX: number | null;
  playResY: number | null;
  styles: Map<string, { color?: string; bold?: boolean; italic?: boolean; alignment?: number; marginL?: number; marginR?: number; marginV?: number }>;
};

function parseAssHeader(codecPrivate?: Uint8Array): AssHeader {
  const header: AssHeader = { playResX: null, playResY: null, styles: new Map() };
  if (!codecPrivate || codecPrivate.byteLength === 0) return header;
  let text: string;
  try { text = new TextDecoder("utf-8", { fatal: true }).decode(codecPrivate); } catch { return header; }
  const section = { name: "", format: [] as string[] };
  for (const raw of text.replace(/\r/g, "").split("\n")) {
    const line = raw.trim();
    const sectionMatch = /^\[([^\]]+)\]$/u.exec(line);
    if (sectionMatch) { section.name = sectionMatch[1].toLowerCase(); section.format = []; continue; }
    const pair = /^([^:]+):\s*(.*)$/u.exec(line);
    if (!pair) continue;
    const key = pair[1].trim().toLowerCase();
    const value = pair[2].trim();
    if (section.name === "script info") {
      if (key === "playresx") header.playResX = positiveNumber(value);
      if (key === "playresy") header.playResY = positiveNumber(value);
    } else if (section.name.includes("styles") && key === "format") {
      section.format = value.split(",").map((part) => part.trim().toLowerCase());
    } else if (section.name.includes("styles") && key === "style") {
      const fields = value.split(",");
      const get = (name: string) => {
        const index = section.format.indexOf(name);
        return index >= 0 ? fields[index]?.trim() : undefined;
      };
      const styleName = get("name");
      if (styleName) header.styles.set(styleName.toLowerCase(), {
        color: assColor(get("primarycolour")),
        bold: assBoolean(get("bold")),
        italic: assBoolean(get("italic")),
        alignment: boundedInteger(get("alignment"), 1, 9),
        marginL: boundedInteger(get("marginl"), 0, 10_000),
        marginR: boundedInteger(get("marginr"), 0, 10_000),
        marginV: boundedInteger(get("marginv"), 0, 10_000),
      });
    }
  }
  return header;
}

function parseDialogue(value: string) {
  const fields = value.split(",");
  if (fields.length < 10) return null;
  const layer = boundedInteger(fields[0], 0, 10_000) ?? 0;
  const style = fields[3]?.trim() || "Default";
  const text = fields.slice(9).join(",");
  let drawing = false;
  for (const match of text.matchAll(/\\p(\d+)/gi)) drawing = Number(match[1]) > 0;
  return { layer, style, text, drawing };
}

function parseStyledText(text: string, header: AssHeader, styleName: string) {
  const base = header.styles.get(styleName.toLowerCase()) ?? {};
  let current = { color: base.color, bold: base.bold, italic: base.italic };
  let alignment = base.alignment;
  let position: { x: number; y: number } | undefined;
  const runs: Array<{ text: string; color?: string; bold?: boolean; italic?: boolean }> = [];
  let plain = "";
  const append = (value: string) => {
    if (!value) return;
    plain += value;
    const previous = runs[runs.length - 1];
    if (previous && previous.color === current.color && previous.bold === current.bold && previous.italic === current.italic) previous.text += value;
    else if (runs.length < MAX_RUNS) runs.push({ text: value, ...current });
  };
  const tagPattern = /\{([^{}]{0,512})\}/gu;
  let cursor = 0;
  for (const match of text.matchAll(tagPattern)) {
    append(text.slice(cursor, match.index));
    for (const tag of match[1].matchAll(/\\([bi]|(?:1?[ca])|an[1-9]|pos\([^)]*\)|r(?:\s+[^\\]+)?)/giu)) {
      const value = tag[1];
      if (/^b$/i.test(value)) current = { ...current, bold: true };
      else if (/^i$/i.test(value)) current = { ...current, italic: true };
      else if (/^1?c/i.test(value)) current = { ...current, color: assColor(value.slice(value.indexOf("c") + 1)) };
      else if (/^1?a/i.test(value)) current = { ...current, color: applyAlpha(current.color, value.slice(value.indexOf("a") + 1)) };
      else if (/^an[1-9]$/i.test(value)) alignment = Number(value.slice(2));
      else if (/^pos\(/i.test(value) && header.playResX && header.playResY) {
        const values = value.slice(4, -1).split(",").map(Number);
        if (values.length === 2 && values.every(Number.isFinite)) position = { x: clamp(values[0] / header.playResX * 100, 0, 100), y: clamp(values[1] / header.playResY * 100, 0, 100) };
      } else if (/^r/i.test(value)) current = { color: base.color, bold: base.bold, italic: base.italic };
    }
    cursor = (match.index ?? cursor) + match[0].length;
  }
  append(text.slice(cursor));
  plain = plain.replace(/\\N|\\n/gu, "\n").replace(/\\h/gu, " ").trim();
  if (!plain) return { text: "", runs: [] as typeof runs };
  return { text: plain, runs, style: current, alignment, position };
}

function assColor(value?: string) {
  const match = value?.match(/&H([0-9a-f]{6,8})/iu);
  if (!match) return undefined;
  const digits = match[1].padStart(8, "0");
  const alpha = 255 - parseInt(digits.slice(0, 2), 16);
  const blue = parseInt(digits.slice(-6, -4), 16);
  const green = parseInt(digits.slice(-4, -2), 16);
  const red = parseInt(digits.slice(-2), 16);
  return `rgba(${red}, ${green}, ${blue}, ${(alpha / 255).toFixed(3)})`;
}
function applyAlpha(color: string | undefined, value: string) { return color ? color.replace(/, [^)]*\)$/u, `, ${(255 - parseInt(value.replace(/&H/iu, "").slice(-2), 16)) / 255})`) : undefined; }
function assBoolean(value?: string) { return value === "1" ? true : value === "0" ? false : undefined; }
function positiveNumber(value: string) { const number = Number(value); return Number.isFinite(number) && number > 0 ? number : null; }
function boundedInteger(value: string | undefined, min: number, max: number) { const number = Number(value); return Number.isInteger(number) && number >= min && number <= max ? number : undefined; }
function clamp(value: number, min: number, max: number) { return Math.max(min, Math.min(max, value)); }
