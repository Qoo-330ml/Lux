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

/** The single source of truth for the hero backdrop: fanart first, then the poster. */
export function heroBackdropUrl(item: MediaItem | undefined, variantsEnabled: boolean): string | undefined {
  if (!item) return undefined;
  const width = variantsEnabled ? HERO_BACKDROP_VARIANT_WIDTH : undefined;
  return imageUrl(item, "fanart", width) ?? imageUrl(item, "poster", width);
}

export function nextHeroBackdropUrl(
  items: MediaItem[],
  activeIndex: number,
  variantsEnabled: boolean,
): string | undefined {
  if (items.length < 2) return undefined;
  const nextIndex = (activeIndex + 1) % items.length;
  return heroBackdropUrl(items[nextIndex], variantsEnabled);
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
}

export function useHeroBackdropPreload({ items, activeIndex, variantsEnabled }: HeroBackdropPreloadOptions) {
  const nextUrl = useMemo(
    () => nextHeroBackdropUrl(items, activeIndex, variantsEnabled),
    [items, activeIndex, variantsEnabled],
  );

  useEffect(() => {
    if (!nextUrl) return undefined;
    const controller = new AbortController();
    void preloadHeroBackdrop(nextUrl, controller.signal);
    return () => controller.abort();
  }, [nextUrl]);
}
