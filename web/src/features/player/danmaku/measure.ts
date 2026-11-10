/**
 * Text width measurement for danmaku layout.
 *
 * Guessing width from `text.length * fontSize` misjudges wide CJK text, emoji, and mixed scripts,
 * which made scrolling danmaku overlap. Real measurement (offscreen canvas `measureText`) is used
 * when a 2D context exists, cached per (font size, text), and falls back to the previous estimate in
 * non-DOM/​jsdom environments and for tests that inject their own measurer.
 */

export type MeasureText = (text: string, fontSize: number) => number;

export const DANMAKU_FONT_FAMILY = 'system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif';

const CACHE_LIMIT = 4_000;

let cache = new Map<string, number>();
type MeasuringContext = {
  font: string;
  measureText(text: string): { width: number };
};

let canvasContext: MeasuringContext | null | undefined;

function isJsdom() {
  return typeof navigator !== "undefined" && /jsdom/i.test(navigator.userAgent);
}

function getCanvasContext(): MeasuringContext | null {
  if (canvasContext !== undefined) return canvasContext;
  // jsdom ships no canvas backend; asking for a context would only log "Not implemented".
  if (typeof document === "undefined" || isJsdom()) {
    canvasContext = null;
    return canvasContext;
  }
  try {
    if (typeof OffscreenCanvas !== "undefined") {
      canvasContext = new OffscreenCanvas(8, 8).getContext("2d");
      return canvasContext;
    }
    const canvas = document.createElement("canvas");
    canvas.width = 8;
    canvas.height = 8;
    canvasContext = canvas.getContext("2d");
  } catch {
    canvasContext = null;
  }
  return canvasContext;
}

/** Previous approximation, kept only as a fallback when real measurement is unavailable. */
export function estimateTextWidth(text: string, fontSize: number): number {
  return text.length * fontSize * 0.95;
}

export function createTextMeasurer(): MeasureText {
  return (text, fontSize) => {
    const key = `${fontSize}\u0000${text}`;
    const cached = cache.get(key);
    if (cached !== undefined) return cached;

    let width = Number.NaN;
    const context = getCanvasContext();
    if (context) {
      try {
        context.font = `600 ${fontSize}px ${DANMAKU_FONT_FAMILY}`;
        width = context.measureText(text).width;
      } catch {
        width = Number.NaN;
      }
    }
    if (!Number.isFinite(width) || width <= 0) {
      width = estimateTextWidth(text, fontSize);
    }
    if (cache.size >= CACHE_LIMIT) cache.clear();
    cache.set(key, width);
    return width;
  };
}

/** Drops the measurement cache (viewport/font changes, tests). */
export function clearTextMeasurementCache() {
  cache = new Map();
}
