import { useEffect, useMemo } from "react";
import type { MediaItem } from "../../lib/api/types";
import { imageUrl } from "./media-display";

/**
 * Backdrop warm-up for the home hero.
 *
 * Switching slides swaps the `src` of the hero backdrop, so the next image is warmed in the
 * background while the current one is visible. Only the immediate next slide is preloaded and
 * failures are swallowed: a cold cache costs a flash, never an error.
 */

export const HERO_BACKDROP_VARIANT_WIDTH = 1280;

export type HeroOrientation = "landscape" | "portrait";

/**
 * Width / height below which the hero box is tall enough that a portrait poster crops less than a
 * 16:9 backdrop does (the geometric mean of 2:3 and 16:9 is about 1.09).
 */
export const HERO_PORTRAIT_MAX_ASPECT = 1.1;
/** Dead band around the threshold so a slide with a longer synopsis cannot flip the artwork. */
const HERO_ORIENTATION_HYSTERESIS = 0.05;

/** Orientation of the hero box. An unmeasurable box counts as landscape (the desktop default). */
export function heroOrientation(width: number, height: number, previous?: HeroOrientation): HeroOrientation {
  if (!(width > 0 && height > 0)) return "landscape";
  const aspect = width / height;
  if (previous === "portrait" && aspect < HERO_PORTRAIT_MAX_ASPECT + HERO_ORIENTATION_HYSTERESIS) return "portrait";
  if (previous === "landscape" && aspect >= HERO_PORTRAIT_MAX_ASPECT - HERO_ORIENTATION_HYSTERESIS) return "landscape";
  return aspect < HERO_PORTRAIT_MAX_ASPECT ? "portrait" : "landscape";
}

/**
 * The single source of truth for the hero artwork. A landscape box takes fanart first, then the
 * poster; a portrait box takes the poster first, then the landscape images.
 */
export function heroBackdropUrl(
  item: MediaItem | undefined,
  variantsEnabled: boolean,
  orientation: HeroOrientation = "landscape",
): string | undefined {
  if (!item) return undefined;
  const width = variantsEnabled ? HERO_BACKDROP_VARIANT_WIDTH : undefined;
  if (orientation === "portrait") {
    return imageUrl(item, "poster", width) ?? imageUrl(item, "fanart", width);
  }
  return imageUrl(item, "fanart", width) ?? imageUrl(item, "poster", width);
}

export function nextHeroBackdropUrl(
  items: MediaItem[],
  activeIndex: number,
  variantsEnabled: boolean,
  orientation: HeroOrientation = "landscape",
): string | undefined {
  if (items.length < 2) return undefined;
  const nextIndex = (activeIndex + 1) % items.length;
  return heroBackdropUrl(items[nextIndex], variantsEnabled, orientation);
}

/** Warm the browser cache for `url`. Never rejects and never renders an element. */
export function preloadHeroBackdrop(url: string, signal?: AbortSignal): Promise<boolean> {
  if (signal?.aborted || typeof Image === "undefined") return Promise.resolve(false);
  let image: HTMLImageElement;
  try {
    image = new Image();
  } catch {
    return Promise.resolve(false);
  }
  return new Promise<boolean>((resolve) => {
    let settled = false;
    const settle = (loaded: boolean) => {
      if (settled) return;
      settled = true;
      signal?.removeEventListener("abort", onAbort);
      resolve(loaded);
    };
    const onAbort = () => settle(false);
    signal?.addEventListener("abort", onAbort);
    image.decoding = "async";
    image.onload = () => settle(true);
    image.onerror = () => settle(false);
    image.src = url;
    if (typeof image.decode === "function") {
      image.decode().then(() => settle(true), () => settle(false));
    }
  });
}

export interface HeroBackdropPreloadOptions {
  items: MediaItem[];
  activeIndex: number;
  variantsEnabled: boolean;
  /** `undefined` while the hero box is not measured yet: nothing is warmed for a guess. */
  orientation: HeroOrientation | undefined;
}

export function useHeroBackdropPreload({ items, activeIndex, variantsEnabled, orientation }: HeroBackdropPreloadOptions) {
  const nextUrl = useMemo(
    () => (orientation ? nextHeroBackdropUrl(items, activeIndex, variantsEnabled, orientation) : undefined),
    [items, activeIndex, variantsEnabled, orientation],
  );

  useEffect(() => {
    if (!nextUrl) return undefined;
    const controller = new AbortController();
    void preloadHeroBackdrop(nextUrl, controller.signal);
    return () => controller.abort();
  }, [nextUrl]);
}
