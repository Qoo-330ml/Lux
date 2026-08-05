// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { HorizontalScrollRail } from "../src/components/layout/HorizontalScrollRail";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("HorizontalScrollRail", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
    vi.restoreAllMocks();
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
    });
    expect(container.querySelector("[aria-label=\"向右滚动媒体库\"]")).not.toBeNull();

    await act(async () => {
      container?.querySelector<HTMLButtonElement>("[aria-label=\"向右滚动媒体库\"]")?.click();
    });

    expect(rail?.scrollLeft).toBe(400);
    expect(container.querySelector("[aria-label=\"向右滚动媒体库\"]")).toBeNull();
  });

  it("uses the last card's visible edge when scroll metrics have not caught up", async () => {
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
      rail?.dispatchEvent(new Event("scroll"));
    });

    expect(container.querySelector("[aria-label=\"向右滚动媒体库\"]")).not.toBeNull();
  });
});
