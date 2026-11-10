import { useEffect, useRef, useState } from "react";
import { HERO_CAROUSEL_INTERVAL_MS } from "./carousel";

/**
 * Pause/resume bookkeeping for the home hero carousel.
 *
 * The autoplay countdown is deliberately not restarted from scratch when the user stops
 * hovering or focusing the hero: the remaining milliseconds are carried over so that the
 * progress dot and the actual slide change stay in step (WCAG 2.2.2).
 */

export function nextCarouselIndex(index: number, count: number) {
  if (count <= 0) return 0;
  return (index + 1) % count;
}

export interface HeroCarouselAutoplayOptions {
  /** Number of slides; fewer than two slides never schedule a change. */
  slideCount: number;
  /** True while any pause source (hover, focus-within, hidden document) is active. */
  paused: boolean;
  /** Changes whenever the active slide changes, so the countdown restarts in full. */
  restartToken: string | number;
  onAdvance: () => void;
  intervalMs?: number;
}

export function useHeroCarouselAutoplay({
  slideCount,
  paused,
  restartToken,
  onAdvance,
  intervalMs = HERO_CAROUSEL_INTERVAL_MS,
}: HeroCarouselAutoplayOptions) {
  const remainingRef = useRef(intervalMs);
  const onAdvanceRef = useRef(onAdvance);

  useEffect(() => {
    onAdvanceRef.current = onAdvance;
  });

  // A new slide (or a new interval) always gets the full countdown.
  useEffect(() => {
    remainingRef.current = intervalMs;
  }, [restartToken, intervalMs, slideCount]);

  useEffect(() => {
    if (slideCount < 2 || paused) return undefined;
    let settled = false;
    const startedAt = Date.now();
    const delay = clampDelay(remainingRef.current, intervalMs);
    const timeout = window.setTimeout(() => {
      settled = true;
      remainingRef.current = intervalMs;
      onAdvanceRef.current();
    }, delay);
    return () => {
      window.clearTimeout(timeout);
      if (settled) return;
      const elapsed = Math.max(0, Date.now() - startedAt);
      remainingRef.current = clampDelay(remainingRef.current - elapsed, intervalMs);
    };
  }, [paused, slideCount, intervalMs, restartToken]);
}

function clampDelay(remaining: number, intervalMs: number) {
  if (!Number.isFinite(remaining)) return intervalMs;
  return Math.max(0, Math.min(intervalMs, remaining));
}

/** Track `document.visibilityState`, so a backgrounded tab pauses the carousel. */
export function useDocumentHidden() {
  const [hidden, setHidden] = useState(isDocumentHidden);

  useEffect(() => {
    const update = () => setHidden(isDocumentHidden());
    update();
    document.addEventListener("visibilitychange", update);
    return () => document.removeEventListener("visibilitychange", update);
  }, []);

  return hidden;
}

function isDocumentHidden() {
  return typeof document !== "undefined" && document.visibilityState === "hidden";
}
