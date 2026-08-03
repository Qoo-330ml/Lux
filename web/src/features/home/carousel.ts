import type { HomeResponse, MediaItem } from "../../lib/api/types";

export const HERO_CAROUSEL_INTERVAL_MS = 8_000;

export function heroSlides(home: Pick<HomeResponse, "continueWatching" | "recentlyAdded">) {
  const unique = new Map<string, MediaItem>();
  for (const item of [...(home.continueWatching ?? []), ...(home.recentlyAdded ?? [])]) {
    if (!unique.has(item.id)) unique.set(item.id, item);
  }
  return [...unique.values()];
}
