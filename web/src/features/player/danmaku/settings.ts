/**
 * Danmaku user settings (browser-local).
 *
 * These preferences are deliberately local-only: they are never uploaded, never part of the Lux API,
 * and are not shared between devices. Everything here is pure except `readDanmakuSettings` /
 * `saveDanmakuSettings`, which take an injected `Storage` so they can be tested without a DOM and so
 * a browser with disabled storage (private mode, quota, sandbox) degrades to defaults instead of
 * throwing into the player.
 *
 * Validation is total: `normalizeDanmakuSettings` accepts `unknown` and returns a usable settings
 * object for every input, including corrupted JSON payloads. Stored data from a future or older
 * version therefore never breaks playback; unknown fields are ignored and out-of-range numbers are
 * clamped and snapped to their documented step.
 */

import type { DanmakuMode } from "./parse";

export type DanmakuArea = "full" | "half" | "quarter";

export type DanmakuSettings = {
  /** Rendered opacity, 10%–100% (stored as 0.1–1). */
  opacity: number;
  /** Text size multiplier, 0.5–2.0. */
  fontScale: number;
  /** Scrolling speed multiplier, 0.5–2.0 (scales the scroll crossing duration). */
  speed: number;
  /** Vertical band the scrolling and top danmaku may use. */
  area: DanmakuArea;
  /** Danmaku modes the viewer asked to hide. */
  blockedModes: DanmakuMode[];
  /** Case-insensitive plain substrings; never interpreted as regular expressions. */
  keywords: string[];
  /** Media-time offset in seconds: positive shows danmaku earlier, negative later. */
  offsetSeconds: number;
};

export const DANMAKU_SETTINGS_STORAGE_KEY = "lux.danmaku.settings.v1";

export const DANMAKU_SETTINGS_LIMITS = {
  opacityMin: 0.1,
  opacityMax: 1,
  opacityStep: 0.05,
  fontScaleMin: 0.5,
  fontScaleMax: 2,
  fontScaleStep: 0.1,
  speedMin: 0.5,
  speedMax: 2,
  speedStep: 0.1,
  offsetMin: -30,
  offsetMax: 30,
  offsetStep: 0.5,
  maxKeywords: 50,
  maxKeywordLength: 30,
} as const;

export const DEFAULT_DANMAKU_SETTINGS: DanmakuSettings = {
  opacity: 1,
  fontScale: 1,
  speed: 1,
  area: "full",
  blockedModes: [],
  keywords: [],
  offsetSeconds: 0,
};

/** Scheduler-facing subset: opacity is applied by the CSS container, not by lane assignment. */
export type DanmakuLayoutOptions = {
  area: DanmakuArea;
  fontScale: number;
  speed: number;
  offsetSeconds: number;
  blockedModes: DanmakuMode[];
  /** Lower-cased keywords, ready for `text.toLowerCase().includes(keyword)`. */
  keywords: string[];
};

export const DEFAULT_DANMAKU_LAYOUT: DanmakuLayoutOptions = {
  area: "full",
  fontScale: 1,
  speed: 1,
  offsetSeconds: 0,
  blockedModes: [],
  keywords: [],
};

const AREAS: readonly DanmakuArea[] = ["full", "half", "quarter"];
const MODES: readonly DanmakuMode[] = ["scroll", "top", "bottom"];
const CONTROL_CHARACTER_PATTERN = /[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F]/u;

export function normalizeDanmakuSettings(input: unknown): DanmakuSettings {
  const source = isRecord(input) ? input : {};
  return {
    opacity: quantize(
      source.opacity,
      DANMAKU_SETTINGS_LIMITS.opacityMin,
      DANMAKU_SETTINGS_LIMITS.opacityMax,
      DANMAKU_SETTINGS_LIMITS.opacityStep,
      DEFAULT_DANMAKU_SETTINGS.opacity,
    ),
    fontScale: quantize(
      source.fontScale,
      DANMAKU_SETTINGS_LIMITS.fontScaleMin,
      DANMAKU_SETTINGS_LIMITS.fontScaleMax,
      DANMAKU_SETTINGS_LIMITS.fontScaleStep,
      DEFAULT_DANMAKU_SETTINGS.fontScale,
    ),
    speed: quantize(
      source.speed,
      DANMAKU_SETTINGS_LIMITS.speedMin,
      DANMAKU_SETTINGS_LIMITS.speedMax,
      DANMAKU_SETTINGS_LIMITS.speedStep,
      DEFAULT_DANMAKU_SETTINGS.speed,
    ),
    area: isArea(source.area) ? source.area : DEFAULT_DANMAKU_SETTINGS.area,
    blockedModes: normalizeBlockedModes(source.blockedModes),
    keywords: normalizeDanmakuKeywords(source.keywords),
    offsetSeconds: quantize(
      source.offsetSeconds,
      DANMAKU_SETTINGS_LIMITS.offsetMin,
      DANMAKU_SETTINGS_LIMITS.offsetMax,
      DANMAKU_SETTINGS_LIMITS.offsetStep,
      DEFAULT_DANMAKU_SETTINGS.offsetSeconds,
    ),
  };
}

/**
 * A single blocked keyword: trimmed, non-empty, at most 30 characters, and free of control
 * characters. Returns `null` for anything the UI must not accept (so the form can reject instead of
 * silently storing a truncated value).
 */
export function normalizeDanmakuKeyword(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const trimmed = value.trim();
  if (!trimmed) return null;
  if (trimmed.length > DANMAKU_SETTINGS_LIMITS.maxKeywordLength) return null;
  if (CONTROL_CHARACTER_PATTERN.test(trimmed)) return null;
  return trimmed;
}

/** Deduplicates case-insensitively, keeps the first spelling, and caps the list at 50 entries. */
export function normalizeDanmakuKeywords(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  const seen = new Set<string>();
  const keywords: string[] = [];
  for (const item of value) {
    const keyword = normalizeDanmakuKeyword(item);
    if (!keyword) continue;
    const key = keyword.toLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    keywords.push(keyword);
    if (keywords.length >= DANMAKU_SETTINGS_LIMITS.maxKeywords) break;
  }
  return keywords;
}

export function readDanmakuSettings(storage?: Storage | null): DanmakuSettings {
  const target = storage === undefined ? defaultStorage() : storage;
  if (!target) return defaultSettings();
  try {
    const raw = target.getItem(DANMAKU_SETTINGS_STORAGE_KEY);
    if (!raw) return defaultSettings();
    return normalizeDanmakuSettings(JSON.parse(raw));
  } catch {
    // Corrupted JSON, a disabled storage, or a throwing getter: defaults, never an exception.
    return defaultSettings();
  }
}

/** Saves the normalized settings and returns them, so callers can use the write as their state. */
export function saveDanmakuSettings(
  settings: DanmakuSettings,
  storage?: Storage | null,
): DanmakuSettings {
  const normalized = normalizeDanmakuSettings(settings);
  const target = storage === undefined ? defaultStorage() : storage;
  if (!target) return normalized;
  try {
    target.setItem(DANMAKU_SETTINGS_STORAGE_KEY, JSON.stringify(normalized));
  } catch {
    // Preferences are best-effort when browser storage is unavailable or full.
  }
  return normalized;
}

export function danmakuLayoutFromSettings(settings: DanmakuSettings): DanmakuLayoutOptions {
  const normalized = normalizeDanmakuSettings(settings);
  return {
    area: normalized.area,
    fontScale: normalized.fontScale,
    speed: normalized.speed,
    offsetSeconds: normalized.offsetSeconds,
    blockedModes: [...normalized.blockedModes],
    keywords: normalized.keywords.map((keyword) => keyword.toLowerCase()),
  };
}

export function danmakuLayoutsEqual(
  left: DanmakuLayoutOptions,
  right: DanmakuLayoutOptions,
): boolean {
  return left.area === right.area
    && left.fontScale === right.fontScale
    && left.speed === right.speed
    && left.offsetSeconds === right.offsetSeconds
    && left.blockedModes.length === right.blockedModes.length
    && left.blockedModes.every((mode, index) => right.blockedModes[index] === mode)
    && left.keywords.length === right.keywords.length
    && left.keywords.every((keyword, index) => right.keywords[index] === keyword);
}

/**
 * A default copy with its own arrays: callers receive a settings object they may mutate (the panel
 * draft, tests) without corrupting `DEFAULT_DANMAKU_SETTINGS` or a later reader's value.
 */
function defaultSettings(): DanmakuSettings {
  return {
    ...DEFAULT_DANMAKU_SETTINGS,
    blockedModes: [],
    keywords: [],
  };
}

function normalizeBlockedModes(value: unknown): DanmakuMode[] {
  if (!Array.isArray(value)) return [];
  const modes: DanmakuMode[] = [];
  for (const item of value) {
    if (!isMode(item) || modes.includes(item)) continue;
    modes.push(item);
  }
  return modes;
}

function isArea(value: unknown): value is DanmakuArea {
  return typeof value === "string" && (AREAS as readonly string[]).includes(value);
}

function isMode(value: unknown): value is DanmakuMode {
  return typeof value === "string" && (MODES as readonly string[]).includes(value);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** Clamps to the documented range and snaps onto the step grid (0.35, 1.3, 3.5, …). */
function quantize(
  value: unknown,
  min: number,
  max: number,
  step: number,
  fallback: number,
): number {
  if (typeof value !== "number" || !Number.isFinite(value)) return fallback;
  const bounded = Math.min(max, Math.max(min, value));
  const steps = Math.round((bounded - min) / step);
  return Number((min + steps * step).toFixed(4));
}

function defaultStorage(): Storage | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage;
  } catch {
    // A browser that throws on `localStorage` access must not break the player.
    return null;
  }
}
