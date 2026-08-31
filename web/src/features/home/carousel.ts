import type { HomeResponse, MediaItem } from "../../lib/api/types";

export const HERO_CAROUSEL_INTERVAL_MS = 8_000;
export const HERO_CAROUSEL_MAX_SLIDES = 7;

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
    if (!unique.has(item.id)) unique.set(item.id, item);
  }
  return [...unique.values()].slice(0, HERO_CAROUSEL_MAX_SLIDES);
}
