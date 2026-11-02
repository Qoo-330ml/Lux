// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { HERO_CAROUSEL_INTERVAL_MS } from "../src/features/home/carousel";
import {
  nextCarouselIndex,
  useDocumentHidden,
  useHeroCarouselAutoplay,
} from "../src/features/home/hero-carousel-autoplay";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

type AutoplayProbeProps = {
  slideCount?: number;
  paused?: boolean;
  restartToken?: number;
  intervalMs?: number;
  onAdvance: () => void;
};

function AutoplayProbe({
  slideCount = 3,
  paused = false,
  restartToken = 0,
  intervalMs = HERO_CAROUSEL_INTERVAL_MS,
  onAdvance,
}: AutoplayProbeProps) {
  useHeroCarouselAutoplay({ slideCount, paused, restartToken, intervalMs, onAdvance });
  return null;
}

function HiddenProbe() {
  return <span data-testid="hidden-probe" data-hidden={useDocumentHidden() ? "yes" : "no"} />;
}

describe("nextCarouselIndex", () => {
  it("wraps to the first slide after the last one", () => {
    expect(nextCarouselIndex(0, 3)).toBe(1);
    expect(nextCarouselIndex(2, 3)).toBe(0);
  });

  it("stays on the first slide when there is nothing to advance", () => {
    expect(nextCarouselIndex(0, 0)).toBe(0);
    expect(nextCarouselIndex(4, 0)).toBe(0);
  });
});

describe("useHeroCarouselAutoplay", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;

  function mount(props: AutoplayProbeProps) {
    if (!container) {
      container = document.createElement("div");
      document.body.append(container);
      root = createRoot(container);
    }
    act(() => root?.render(<AutoplayProbe {...props} />));
  }

  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    if (root) {
      act(() => root?.unmount());
      container?.remove();
    }
    root = undefined;
    container = undefined;
    vi.useRealTimers();
  });

  it("advances once the full interval elapses", () => {
    const onAdvance = vi.fn();
    mount({ onAdvance });

    act(() => vi.advanceTimersByTime(HERO_CAROUSEL_INTERVAL_MS - 1));
    expect(onAdvance).not.toHaveBeenCalled();

    act(() => vi.advanceTimersByTime(1));
    expect(onAdvance).toHaveBeenCalledTimes(1);
  });

  it("resumes from the remaining time instead of restarting the interval", () => {
    const onAdvance = vi.fn();
    mount({ onAdvance });
    act(() => vi.advanceTimersByTime(3_000));

    mount({ onAdvance, paused: true });
    act(() => vi.advanceTimersByTime(60_000));
    expect(onAdvance).not.toHaveBeenCalled();

    mount({ onAdvance, paused: false });
    act(() => vi.advanceTimersByTime(HERO_CAROUSEL_INTERVAL_MS - 3_000 - 1));
    expect(onAdvance).not.toHaveBeenCalled();

    act(() => vi.advanceTimersByTime(1));
    expect(onAdvance).toHaveBeenCalledTimes(1);
  });

  it("does not start the countdown while paused", () => {
    const onAdvance = vi.fn();
    mount({ onAdvance, paused: true });

    act(() => vi.advanceTimersByTime(HERO_CAROUSEL_INTERVAL_MS * 3));
    expect(onAdvance).not.toHaveBeenCalled();

    mount({ onAdvance, paused: false });
    act(() => vi.advanceTimersByTime(HERO_CAROUSEL_INTERVAL_MS));
    expect(onAdvance).toHaveBeenCalledTimes(1);
  });

  it("restarts a full interval when the slide changes", () => {
    const onAdvance = vi.fn();
    mount({ onAdvance });
    act(() => vi.advanceTimersByTime(7_000));

    mount({ onAdvance, restartToken: 1 });
    act(() => vi.advanceTimersByTime(HERO_CAROUSEL_INTERVAL_MS - 1));
    expect(onAdvance).not.toHaveBeenCalled();

    act(() => vi.advanceTimersByTime(1));
    expect(onAdvance).toHaveBeenCalledTimes(1);
  });

  it("stays idle when there is nothing to rotate through", () => {
    const onAdvance = vi.fn();
    mount({ onAdvance, slideCount: 1 });

    act(() => vi.advanceTimersByTime(HERO_CAROUSEL_INTERVAL_MS * 3));
    expect(onAdvance).not.toHaveBeenCalled();
  });

  it("stops scheduling after unmount", () => {
    const onAdvance = vi.fn();
    mount({ onAdvance });
    act(() => root?.unmount());
    root = undefined;

    act(() => vi.advanceTimersByTime(HERO_CAROUSEL_INTERVAL_MS * 3));
    expect(onAdvance).not.toHaveBeenCalled();
  });
});

describe("useDocumentHidden", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;

  beforeEach(() => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
    root = undefined;
    container = undefined;
    setVisibility("visible");
  });

  it("follows the page visibility state", () => {
    act(() => root?.render(<HiddenProbe />));
    expect(container?.querySelector("[data-testid='hidden-probe']")?.getAttribute("data-hidden")).toBe("no");

    act(() => {
      setVisibility("hidden");
      document.dispatchEvent(new Event("visibilitychange"));
    });
    expect(container?.querySelector("[data-testid='hidden-probe']")?.getAttribute("data-hidden")).toBe("yes");

    act(() => {
      setVisibility("visible");
      document.dispatchEvent(new Event("visibilitychange"));
    });
    expect(container?.querySelector("[data-testid='hidden-probe']")?.getAttribute("data-hidden")).toBe("no");
  });
});

function setVisibility(state: "visible" | "hidden") {
  Object.defineProperty(document, "visibilityState", { configurable: true, get: () => state });
}
