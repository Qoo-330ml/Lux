import { describe, expect, it } from "vitest";
import {
  DANMAKU_SETTINGS_LIMITS,
  DANMAKU_SETTINGS_STORAGE_KEY,
  DEFAULT_DANMAKU_SETTINGS,
  danmakuLayoutFromSettings,
  normalizeDanmakuKeyword,
  normalizeDanmakuKeywords,
  normalizeDanmakuSettings,
  readDanmakuSettings,
  saveDanmakuSettings,
  type DanmakuSettings,
} from "../src/features/player/danmaku/settings";

/** Minimal in-memory Storage so the settings module can be tested without a DOM. */
function memoryStorage(initial: Record<string, string> = {}): Storage {
  const values = new Map(Object.entries(initial));
  return {
    get length() {
      return values.size;
    },
    clear() {
      values.clear();
    },
    getItem(key: string) {
      return values.get(key) ?? null;
    },
    key(index: number) {
      return [...values.keys()][index] ?? null;
    },
    removeItem(key: string) {
      values.delete(key);
    },
    setItem(key: string, value: string) {
      values.set(key, value);
    },
  };
}

describe("LuxPlayer danmaku settings", () => {
  it("ships versioned defaults that match the documented user settings", () => {
    expect(DANMAKU_SETTINGS_STORAGE_KEY).toBe("lux.danmaku.settings.v1");
    expect(DEFAULT_DANMAKU_SETTINGS).toEqual<DanmakuSettings>({
      opacity: 1,
      fontScale: 1,
      speed: 1,
      area: "full",
      blockedModes: [],
      keywords: [],
      offsetSeconds: 0,
    });
    expect(DANMAKU_SETTINGS_LIMITS.maxKeywords).toBe(50);
    expect(DANMAKU_SETTINGS_LIMITS.maxKeywordLength).toBe(30);
    expect(DANMAKU_SETTINGS_LIMITS.offsetMin).toBe(-30);
    expect(DANMAKU_SETTINGS_LIMITS.offsetMax).toBe(30);
    expect(DANMAKU_SETTINGS_LIMITS.offsetStep).toBe(0.5);
  });

  it("falls back to defaults for corrupted, missing, or non-object storage values", () => {
    const storage = memoryStorage();
    expect(readDanmakuSettings(storage)).toEqual(DEFAULT_DANMAKU_SETTINGS);

    storage.setItem(DANMAKU_SETTINGS_STORAGE_KEY, "{not json");
    expect(readDanmakuSettings(storage)).toEqual(DEFAULT_DANMAKU_SETTINGS);

    storage.setItem(DANMAKU_SETTINGS_STORAGE_KEY, "null");
    expect(readDanmakuSettings(storage)).toEqual(DEFAULT_DANMAKU_SETTINGS);

    storage.setItem(DANMAKU_SETTINGS_STORAGE_KEY, "[1,2,3]");
    expect(readDanmakuSettings(storage)).toEqual(DEFAULT_DANMAKU_SETTINGS);

    storage.setItem(DANMAKU_SETTINGS_STORAGE_KEY, JSON.stringify({ opacity: "0.5", area: 7 }));
    expect(readDanmakuSettings(storage)).toEqual(DEFAULT_DANMAKU_SETTINGS);
  });

  it("never throws when browser storage is unavailable or throws on access", () => {
    expect(readDanmakuSettings(null)).toEqual(DEFAULT_DANMAKU_SETTINGS);
    expect(() => saveDanmakuSettings(DEFAULT_DANMAKU_SETTINGS, null)).not.toThrow();

    const broken = {
      getItem() {
        throw new Error("denied");
      },
      setItem() {
        throw new Error("quota");
      },
    } as unknown as Storage;
    expect(readDanmakuSettings(broken)).toEqual(DEFAULT_DANMAKU_SETTINGS);
    expect(() => saveDanmakuSettings(DEFAULT_DANMAKU_SETTINGS, broken)).not.toThrow();
  });

  it("clamps out-of-range numbers and snaps them to their documented steps", () => {
    const normalized = normalizeDanmakuSettings({
      opacity: 0.37,
      fontScale: 1.26,
      speed: 9,
      offsetSeconds: 3.7,
    });

    expect(normalized.opacity).toBeCloseTo(0.35, 5);
    expect(normalized.fontScale).toBeCloseTo(1.3, 5);
    expect(normalized.speed).toBe(2);
    expect(normalized.offsetSeconds).toBe(3.5);

    const bounded = normalizeDanmakuSettings({
      opacity: 5,
      fontScale: -3,
      speed: 0.1,
      offsetSeconds: 1000,
    });
    expect(bounded.opacity).toBe(1);
    expect(bounded.fontScale).toBe(0.5);
    expect(bounded.speed).toBe(0.5);
    expect(bounded.offsetSeconds).toBe(30);

    expect(normalizeDanmakuSettings({ offsetSeconds: -1000 }).offsetSeconds).toBe(-30);
  });

  it("keeps only known display areas and mode blocks", () => {
    expect(normalizeDanmakuSettings({ area: "quarter" }).area).toBe("quarter");
    expect(normalizeDanmakuSettings({ area: "half" }).area).toBe("half");
    expect(normalizeDanmakuSettings({ area: "bottom" }).area).toBe("full");
    expect(normalizeDanmakuSettings({ blockedModes: ["scroll", "bogus", "top", "scroll"] }).blockedModes)
      .toEqual(["scroll", "top"]);
    expect(normalizeDanmakuSettings({ blockedModes: "scroll" }).blockedModes).toEqual([]);
  });

  it("validates keywords: trimmed, deduplicated case-insensitively, capped and length-limited", () => {
    expect(normalizeDanmakuKeyword("  前排  ")).toBe("前排");
    expect(normalizeDanmakuKeyword("   ")).toBeNull();
    expect(normalizeDanmakuKeyword(12)).toBeNull();
    expect(normalizeDanmakuKeyword("x".repeat(30))).toBe("x".repeat(30));
    expect(normalizeDanmakuKeyword("x".repeat(31))).toBeNull();

    expect(normalizeDanmakuKeywords(["Foo", " foo ", "BAR", 3, "", "x".repeat(31)])).toEqual(["Foo", "BAR"]);

    const many = Array.from({ length: 60 }, (_, index) => `keyword-${index}`);
    const capped = normalizeDanmakuKeywords(many);
    expect(capped).toHaveLength(50);
    expect(capped[0]).toBe("keyword-0");
    expect(capped[49]).toBe("keyword-49");
  });

  it("round-trips through storage and returns the normalized value from save", () => {
    const storage = memoryStorage();
    const saved = saveDanmakuSettings(
      {
        ...DEFAULT_DANMAKU_SETTINGS,
        opacity: 0.55,
        area: "half",
        blockedModes: ["bottom"],
        keywords: ["  Hello  "],
        offsetSeconds: -1.5,
      },
      storage,
    );

    expect(saved.opacity).toBeCloseTo(0.55, 5);
    expect(readDanmakuSettings(storage)).toEqual(saved);
    expect(JSON.parse(storage.getItem(DANMAKU_SETTINGS_STORAGE_KEY)!)).toMatchObject({
      area: "half",
      keywords: ["Hello"],
    });
  });

  it("returns independent default arrays so callers cannot mutate shared defaults", () => {
    const first = readDanmakuSettings(null);
    first.blockedModes.push("scroll");
    first.keywords.push("共享");

    const second = readDanmakuSettings(null);
    expect(second.blockedModes).toEqual([]);
    expect(second.keywords).toEqual([]);
    expect(DEFAULT_DANMAKU_SETTINGS.blockedModes).toEqual([]);
    expect(DEFAULT_DANMAKU_SETTINGS.keywords).toEqual([]);
  });

  it("derives the scheduler layout from user settings", () => {
    const layout = danmakuLayoutFromSettings({
      ...DEFAULT_DANMAKU_SETTINGS,
      area: "quarter",
      fontScale: 1.4,
      speed: 1.7,
      offsetSeconds: 2.5,
      blockedModes: ["top"],
      keywords: ["Spoiler", "剧透"],
    });

    expect(layout).toEqual({
      area: "quarter",
      fontScale: 1.4,
      speed: 1.7,
      offsetSeconds: 2.5,
      blockedModes: ["top"],
      // Keywords are normalized to lower case once, here, so the per-entry match stays cheap.
      keywords: ["spoiler", "剧透"],
    });
  });
});
