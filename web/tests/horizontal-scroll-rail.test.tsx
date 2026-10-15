// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { HorizontalScrollRail } from "../src/components/layout/HorizontalScrollRail";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

type Rect = Pick<DOMRect, "left" | "right" | "top" | "bottom" | "width" | "height">;

class ControlledIntersectionObserver implements IntersectionObserver {
  static instances: ControlledIntersectionObserver[] = [];
  readonly root: Element | Document | null;
  readonly rootMargin: string;
  readonly thresholds: number[];
  readonly targets = new Set<Element>();

  constructor(
    private readonly callback: IntersectionObserverCallback,
    options: IntersectionObserverInit = {},
  ) {
    this.root = options.root ?? null;
    this.rootMargin = options.rootMargin ?? "0px";
    this.thresholds = Array.isArray(options.threshold) ? options.threshold : [options.threshold ?? 0];
    ControlledIntersectionObserver.instances.push(this);
  }

  observe(target: Element) {
    this.targets.add(target);
  }

  unobserve(target: Element) {
    this.targets.delete(target);
  }

  disconnect() {
    this.targets.clear();
  }

  takeRecords() {
    return [];
  }

  emit(entries: Array<{ target: Element; bounds: Rect; rootBounds?: Rect | null }>) {
    const toDomRect = (rect: Rect) => ({ ...rect, x: rect.left, y: rect.top, toJSON: () => ({}) }) as DOMRect;
    this.callback(
      entries.map(({ target, bounds, rootBounds = { left: 0, right: 320, top: 0, bottom: 100, width: 320, height: 100 } }) => ({
        target,
        boundingClientRect: toDomRect(bounds),
        rootBounds: rootBounds ? toDomRect(rootBounds) : null,
        intersectionRect: toDomRect(bounds),
        intersectionRatio: 1,
        isIntersecting: true,
        time: 0,
      }) as IntersectionObserverEntry),
      this,
    );
  }
}

function nextFrame() {
  return new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
}

describe("HorizontalScrollRail", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    ControlledIntersectionObserver.instances = [];
  });

  it("tracks visible edge positions without reading element geometry on scroll", async () => {
    vi.stubGlobal("IntersectionObserver", ControlledIntersectionObserver);
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    await act(async () => {
      root?.render(
        <HorizontalScrollRail ariaLabel="演员列表">
          <div className="rail-content">
            <div className="rail-card">第一张</div>
            <div className="rail-card">第二张</div>
          </div>
        </HorizontalScrollRail>,
      );
    });

    const rail = container.querySelector<HTMLDivElement>(".lux-horizontal-scroll-viewport");
    const firstCard = rail?.firstElementChild?.firstElementChild;
    const lastCard = rail?.firstElementChild?.lastElementChild;
    const observer = ControlledIntersectionObserver.instances[0];
    expect(rail).not.toBeNull();
    expect(firstCard).not.toBeNull();
    expect(lastCard).not.toBeNull();
    expect(observer?.root).toBe(rail);
    expect(observer?.targets.has(firstCard!)).toBe(true);
    expect(observer?.targets.has(lastCard!)).toBe(true);

    await act(async () => {
      observer?.emit([
        { target: firstCard!, bounds: { left: 0, right: 120, top: 0, bottom: 100, width: 120, height: 100 } },
        { target: lastCard!, bounds: { left: 350, right: 500, top: 0, bottom: 100, width: 150, height: 100 } },
      ]);
    });
    expect(container.querySelector('[aria-label="向右滚动演员列表"]')).not.toBeNull();
    expect(container.querySelector('[aria-label="向左滚动演员列表"]')).toBeNull();

    const readRailRect = vi.spyOn(rail!, "getBoundingClientRect");
    const readFirstRect = vi.spyOn(firstCard!, "getBoundingClientRect");
    const readLastRect = vi.spyOn(lastCard!, "getBoundingClientRect");
    await act(async () => {
      rail?.dispatchEvent(new Event("scroll"));
      await nextFrame();
    });
    expect(readRailRect).not.toHaveBeenCalled();
    expect(readFirstRect).not.toHaveBeenCalled();
    expect(readLastRect).not.toHaveBeenCalled();

    await act(async () => {
      observer?.emit([
        { target: firstCard!, bounds: { left: -40, right: 80, top: 0, bottom: 100, width: 120, height: 100 } },
        { target: lastCard!, bounds: { left: 280, right: 430, top: 0, bottom: 100, width: 150, height: 100 } },
      ]);
    });
    expect(container.querySelector('[aria-label="向左滚动演员列表"]')).not.toBeNull();

    await act(async () => {
      observer?.emit([
        { target: firstCard!, bounds: { left: -120, right: 0, top: 0, bottom: 100, width: 120, height: 100 } },
        { target: lastCard!, bounds: { left: 170, right: 320, top: 0, bottom: 100, width: 150, height: 100 } },
      ]);
    });
    expect(container.querySelector('[aria-label="向右滚动演员列表"]')).toBeNull();
  });

  it("shows a right arrow only when the rail overflows and scrolls one viewport", async () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    await act(async () => {
      root?.render(
        <HorizontalScrollRail ariaLabel="演员列表">
          <div>演员卡片</div>
        </HorizontalScrollRail>,
      );
    });

    const rail = container.querySelector<HTMLDivElement>(".lux-horizontal-scroll-viewport");
    expect(rail).not.toBeNull();
    Object.defineProperty(rail, "clientWidth", { configurable: true, value: 320 });
    Object.defineProperty(rail, "scrollWidth", { configurable: true, value: 720 });
    Object.defineProperty(rail, "scrollLeft", { configurable: true, writable: true, value: 0 });
    const scrollTo = vi.fn();
    Object.defineProperty(rail, "scrollTo", { configurable: true, value: scrollTo });

    await act(async () => {
      rail?.dispatchEvent(new Event("scroll"));
      await nextFrame();
    });

    expect(container.querySelector("[aria-label=\"向右滚动演员列表\"]")).not.toBeNull();
    expect(container.querySelector("[aria-label=\"向左滚动演员列表\"]")).toBeNull();

    await act(async () => {
      container?.querySelector<HTMLButtonElement>("[aria-label=\"向右滚动演员列表\"]")?.click();
    });

    expect(scrollTo).toHaveBeenCalledWith({ behavior: "smooth", left: 256 });
  });

  it("shows no arrows when all content fits in the viewport", async () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    await act(async () => {
      root?.render(
        <HorizontalScrollRail ariaLabel="视频轨道">
          <div>视频轨</div>
        </HorizontalScrollRail>,
      );
    });

    const rail = container.querySelector<HTMLDivElement>(".lux-horizontal-scroll-viewport");
    expect(rail).not.toBeNull();
    Object.defineProperty(rail, "clientWidth", { configurable: true, value: 320 });
    Object.defineProperty(rail, "scrollWidth", { configurable: true, value: 320 });

    await act(async () => {
      rail?.dispatchEvent(new Event("scroll"));
      await nextFrame();
    });

    expect(container.querySelector(".lux-horizontal-scroll-arrow")).toBeNull();
  });

  it("hides the right arrow immediately when a click reaches the final scroll position", async () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    await act(async () => {
      root?.render(
        <HorizontalScrollRail ariaLabel="媒体库">
          <div>媒体卡片</div>
        </HorizontalScrollRail>,
      );
    });

    const rail = container.querySelector<HTMLDivElement>(".lux-horizontal-scroll-viewport");
    expect(rail).not.toBeNull();
    Object.defineProperty(rail, "clientWidth", { configurable: true, value: 320 });
    Object.defineProperty(rail, "scrollWidth", { configurable: true, value: 720 });
    Object.defineProperty(rail, "scrollLeft", { configurable: true, writable: true, value: 200 });
    Object.defineProperty(rail, "scrollTo", { configurable: true, value: vi.fn() });

    await act(async () => {
      rail?.dispatchEvent(new Event("scroll"));
      await nextFrame();
    });
    expect(container.querySelector("[aria-label=\"向右滚动媒体库\"]")).not.toBeNull();

    await act(async () => {
      container?.querySelector<HTMLButtonElement>("[aria-label=\"向右滚动媒体库\"]")?.click();
    });

    expect(rail?.scrollLeft).toBe(400);
    expect(container.querySelector("[aria-label=\"向右滚动媒体库\"]")).toBeNull();
  });

  it("uses the last card's visible edge when scroll metrics have not caught up", async () => {
    vi.stubGlobal("IntersectionObserver", ControlledIntersectionObserver);
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    await act(async () => {
      root?.render(
        <HorizontalScrollRail ariaLabel="媒体库">
          <div className="rail-content">
            <div className="rail-card">第一张</div>
            <div className="rail-card">第二张</div>
          </div>
        </HorizontalScrollRail>,
      );
    });

    const rail = container.querySelector<HTMLDivElement>(".lux-horizontal-scroll-viewport");
    const content = rail?.firstElementChild;
    const firstCard = content?.firstElementChild;
    const lastCard = content?.lastElementChild;
    expect(rail).not.toBeNull();
    expect(firstCard).not.toBeNull();
    expect(lastCard).not.toBeNull();
    Object.defineProperty(rail, "clientWidth", { configurable: true, value: 320 });
    Object.defineProperty(rail, "scrollWidth", { configurable: true, value: 320 });
    Object.defineProperty(rail, "scrollLeft", { configurable: true, writable: true, value: 0 });
    const rect = (left: number, right: number) => ({ left, right, top: 0, bottom: 100, width: right - left, height: 100 });
    Object.defineProperty(rail, "getBoundingClientRect", { configurable: true, value: () => rect(0, 320) });
    Object.defineProperty(firstCard, "getBoundingClientRect", { configurable: true, value: () => rect(0, 120) });
    Object.defineProperty(lastCard, "getBoundingClientRect", { configurable: true, value: () => rect(350, 500) });

    await act(async () => {
      ControlledIntersectionObserver.instances[0]?.emit([
        { target: firstCard!, bounds: rect(0, 120) },
        { target: lastCard!, bounds: rect(350, 500) },
      ]);
    });

    expect(container.querySelector("[aria-label=\"向右滚动媒体库\"]")).not.toBeNull();
  });

  it("reconciles the right arrow with the browser-clamped scroll position", async () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    await act(async () => {
      root?.render(
        <HorizontalScrollRail ariaLabel="最近播放">
          <div className="rail-content">
            <div className="rail-card">第一张</div>
            <div className="rail-card">第二张</div>
          </div>
        </HorizontalScrollRail>,
      );
    });

    const rail = container.querySelector<HTMLDivElement>(".lux-horizontal-scroll-viewport");
    const content = rail?.firstElementChild;
    const firstCard = content?.firstElementChild;
    const lastCard = content?.lastElementChild;
    expect(rail).not.toBeNull();
    expect(firstCard).not.toBeNull();
    expect(lastCard).not.toBeNull();
    Object.defineProperty(rail, "clientWidth", { configurable: true, value: 320 });
    Object.defineProperty(rail, "scrollWidth", { configurable: true, value: 720 });
    Object.defineProperty(rail, "scrollLeft", { configurable: true, writable: true, value: 200 });
    const rect = (left: number, right: number) => ({ left, right, top: 0, bottom: 100, width: right - left, height: 100 });
    Object.defineProperty(rail, "getBoundingClientRect", { configurable: true, value: () => rect(0, 320) });
    Object.defineProperty(firstCard, "getBoundingClientRect", {
      configurable: true,
      value: () => rect(-rail.scrollLeft, 120 - rail.scrollLeft),
    });
    Object.defineProperty(lastCard, "getBoundingClientRect", {
      configurable: true,
      value: () => rect(120 - rail.scrollLeft, rail.scrollLeft >= 400 ? 320 : 600),
    });
    const scrollTo = vi.fn(({ left }: ScrollToOptions) => {
      rail.scrollLeft = Math.min(left ?? 0, 400);
      rail.dispatchEvent(new Event("scroll"));
    });
    Object.defineProperty(rail, "scrollTo", { configurable: true, value: scrollTo });

    await act(async () => {
      rail?.dispatchEvent(new Event("scroll"));
      await nextFrame();
    });
    expect(container.querySelector("[aria-label=\"向右滚动最近播放\"]")).not.toBeNull();

    await act(async () => {
      container?.querySelector<HTMLButtonElement>("[aria-label=\"向右滚动最近播放\"]")?.click();
      await nextFrame();
    });

    expect(scrollTo).toHaveBeenCalledWith({ behavior: "smooth", left: 456 });
    expect(rail?.scrollLeft).toBe(400);
    expect(container.querySelector("[aria-label=\"向右滚动最近播放\"]")).toBeNull();
  });
});
