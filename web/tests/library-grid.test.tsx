// @vitest-environment jsdom
import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { LibraryGrid } from "../src/features/library/LibraryGrid";
import type { MediaItem } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const items: MediaItem[] = Array.from({ length: 505 }, (_, index) => ({ id: `item-${index}`, title: `Movie ${index}` }));

describe("LibraryGrid", () => {
  let container: HTMLDivElement;
  let root: Root;
  let columns: number;
  let rowHeight: number;
  const observers = new Set<() => void>();

  beforeEach(() => {
    columns = 5;
    rowHeight = 300;
    vi.stubGlobal("ResizeObserver", class {
      constructor(callback: ResizeObserverCallback) { observers.add(() => callback([], this as unknown as ResizeObserver)); }
      observe() {}
      unobserve() {}
      disconnect() {}
    });
    vi.spyOn(window, "scrollTo").mockImplementation(() => {});
    vi.spyOn(window, "scrollY", "get").mockReturnValue(0);
    const getStyle = window.getComputedStyle;
    vi.spyOn(window, "getComputedStyle").mockImplementation((element) => {
      const style = getStyle(element);
      if (element.classList.contains("lux-library-virtual-grid")) {
        return { ...style, gridTemplateColumns: Array(columns).fill("180px").join(" "), rowGap: "34px", columnGap: "16px" } as CSSStyleDeclaration;
      }
      return style;
    });
    vi.spyOn(Element.prototype, "getBoundingClientRect").mockImplementation(function () {
      return { width: 1000, height: this.classList.contains("lux-library-virtual-row") ? rowHeight : 0, top: 200 - window.scrollY } as DOMRect;
    });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    observers.clear();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  function render(data = items) {
    act(() => root.render(<LibraryGrid items={data} renderItem={(item) => <button>{item.title}</button>} />));
  }

  function scrollTo(top: number) {
    act(() => {
      vi.spyOn(window, "scrollY", "get").mockReturnValue(top);
      window.dispatchEvent(new Event("scroll"));
    });
  }

  it("mounts only visible and buffered rows, and reaches the final incomplete row", () => {
    render();
    expect(container.querySelectorAll("button").length).toBeLessThan(50);
    expect(container.textContent).toContain("Movie 0");
    scrollTo(10_000);
    expect(container.textContent).not.toContain("Movie 0");
    const height = parseFloat(container.querySelector<HTMLElement>(".lux-library-virtual-grid")!.style.height);
    scrollTo(height + 200 - window.innerHeight);
    expect(container.textContent).toContain("Movie 504");
    expect(container.querySelectorAll("button").length).toBeLessThan(50);
    expect(container.querySelector('[aria-posinset="505"]')?.getAttribute("aria-setsize")).toBe("505");
  });

  it("regroups responsive columns and measures row height including badges", () => {
    render();
    act(() => {
      columns = 2;
      rowHeight = 440;
      for (const update of observers) update();
      window.dispatchEvent(new Event("resize"));
    });
    expect(container.querySelector('[data-index="0"]')?.querySelectorAll("button")).toHaveLength(2);
    const row0 = container.querySelector<HTMLElement>('[data-index="0"]')!;
    const row1 = container.querySelector<HTMLElement>('[data-index="1"]')!;
    expect(parseFloat(row1.style.top) - parseFloat(row0.style.top)).toBeGreaterThanOrEqual(440 + 34);
  });

  it("keeps the visible range when pages append and retains a focused card", () => {
    render(items.slice(0, 120));
    act(() => container.querySelector("button")?.focus());
    scrollTo(4_000);
    expect(document.activeElement?.textContent).toBe("Movie 0");
    const before = [...container.querySelectorAll("button")].map((button) => button.textContent);
    render(items);
    expect([...container.querySelectorAll("button")].map((button) => button.textContent)).toEqual(before);
    act(() => (document.activeElement as HTMLElement).blur());
    expect(container.textContent).not.toContain("Movie 0");
  });

  it("preserves parent-owned selection after a row unmounts and remounts", () => {
    function SelectableGrid() {
      const [selected, setSelected] = useState<string>();
      return <LibraryGrid items={items} renderItem={(item) => <button aria-pressed={selected === item.id} onClick={() => setSelected(item.id)}>{item.title}</button>} />;
    }
    act(() => root.render(<SelectableGrid />));
    act(() => container.querySelector("button")?.click());
    scrollTo(10_000);
    expect(container.querySelector('[aria-posinset="1"]')).toBeNull();
    scrollTo(0);
    expect(container.querySelector("button")?.getAttribute("aria-pressed")).toBe("true");
  });
});
