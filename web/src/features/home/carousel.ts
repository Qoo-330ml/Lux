import type { HomeCarouselResponse, HomeResponse, MediaItem } from "../../lib/api/types";

export const HERO_CAROUSEL_INTERVAL_MS = 8_000;
export const HERO_CAROUSEL_MAX_SLIDES = 7;
const HOME_CAROUSEL_CACHE_VERSION = 2;
const HOME_CAROUSEL_CACHE_TTL_MS = 5 * 60_000;
const HOME_CAROUSEL_CACHE_PREFIX = "lux.home-carousel.v2:";
const LEGACY_HOME_CACHE_PREFIX = "lux.home.v1:";

export function readHomeCarouselCache(
  userId: string,
): { data: HomeCarouselResponse; savedAt: number } | undefined {
  if (!userId || typeof window === "undefined") return undefined;
  try {
    window.sessionStorage.removeItem(`${LEGACY_HOME_CACHE_PREFIX}${encodeURIComponent(userId)}`);
    const raw = window.sessionStorage.getItem(homeCarouselCacheKey(userId));
    if (!raw) return undefined;
    const parsed: unknown = JSON.parse(raw);
    if (!isRecord(parsed) || parsed.version !== HOME_CAROUSEL_CACHE_VERSION || !isRecord(parsed.data)) return undefined;
    if (!Array.isArray(parsed.data.recommended)) return undefined;
    if (typeof parsed.savedAt !== "number" || !Number.isFinite(parsed.savedAt)) return undefined;
    if (Date.now() - parsed.savedAt > HOME_CAROUSEL_CACHE_TTL_MS) return undefined;
    return { data: parsed.data as HomeCarouselResponse, savedAt: parsed.savedAt };
  } catch {
    return undefined;
  }
}

export function writeHomeCarouselCache(userId: string, data: HomeCarouselResponse) {
  if (!userId || typeof window === "undefined") return;
  try {
    window.sessionStorage.removeItem(`${LEGACY_HOME_CACHE_PREFIX}${encodeURIComponent(userId)}`);
    window.sessionStorage.setItem(homeCarouselCacheKey(userId), JSON.stringify({
      version: HOME_CAROUSEL_CACHE_VERSION,
      savedAt: Date.now(),
      data: { recommended: data.recommended ?? [] },
    }));
  } catch {
    // Storage can be unavailable in private browsing or when the quota is exhausted.
  }
}

export function clearHomeCarouselCache() {
  if (typeof window === "undefined") return;
  try {
    for (let index = window.sessionStorage.length - 1; index >= 0; index -= 1) {
      const key = window.sessionStorage.key(index);
      if (key?.startsWith(HOME_CAROUSEL_CACHE_PREFIX) || key?.startsWith(LEGACY_HOME_CACHE_PREFIX)) {
        window.sessionStorage.removeItem(key);
      }
    }
  } catch {
    // Storage can be unavailable in private browsing or when the quota is exhausted.
  }
}

function homeCarouselCacheKey(userId: string) {
  return `${HOME_CAROUSEL_CACHE_PREFIX}${encodeURIComponent(userId)}`;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

export type HeroTitleScale = "default" | "compact" | "small";

export function heroTitleScale(title: string): HeroTitleScale {
  const visualLength = [...title.trim()].reduce((length, character) => {
    return length + (/^[\u0000-\u00ff]$/.test(character) ? 0.55 : 1);
  }, 0);

  if (visualLength >= 34) return "small";
  if (visualLength >= 22) return "compact";
  return "default";
}

export function heroSlides(home: Pick<HomeResponse, "recommended" | "continueWatching" | "recentlyAdded">) {
  const unique = new Map<string, MediaItem>();
  for (const item of [
    ...(home.recommended ?? []),
    ...(home.continueWatching ?? []),
    ...(home.recentlyAdded ?? []),
  ]) {
    if (item.itemType !== "MOVIE" && item.itemType !== "SERIES") continue;
    if (!unique.has(item.id)) unique.set(item.id, item);
  }
  return [...unique.values()].slice(0, HERO_CAROUSEL_MAX_SLIDES);
}
