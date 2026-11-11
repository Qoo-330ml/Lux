// @vitest-environment jsdom

import { act, useRef } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { MediaItem } from "../src/lib/api/types";
import {
  heroBackdropUrl,
  heroOrientation,
  nextHeroBackdropUrl,
  preloadHeroBackdrop,
  useHeroBackdropPreload,
} from "../src/features/home/hero-preload";
import { useHeroOrientation } from "../src/features/home/hero-orientation";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

function slide(id: string, imageTags?: Record<string, string>): MediaItem {
  return { id, title: id, itemType: "MOVIE", imageTags };
}

class FakeImage {
  static instances: FakeImage[] = [];
  static decodeResult: "resolve" | "reject" = "resolve";

  src = "";
  decoding = "";
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;

  constructor() {
    FakeImage.instances.push(this);
  }

  decode() {
    return FakeImage.decodeResult === "resolve"
      ? Promise.resolve()
      : Promise.reject(new Error("decode failed"));
  }
}

describe("heroBackdropUrl", () => {
  it("prefers fanart and falls back to the poster", () => {
    expect(heroBackdropUrl(slide("a", { fanart: "fa", poster: "po" }), false))
      .toBe("/api/v1/items/a/images/fanart?tag=fa");
    expect(heroBackdropUrl(slide("a", { poster: "po" }), false))
      .toBe("/api/v1/items/a/images/poster?tag=po");
    expect(heroBackdropUrl(slide("a", { thumb: "th" }), false))
      .toBe("/api/v1/items/a/images/thumb?tag=th");
  });

  it("requests the 1280 variant only when image variants are enabled", () => {
    expect(heroBackdropUrl(slide("a", { fanart: "fa" }), true))
      .toBe("/api/v1/items/a/images/fanart?tag=fa&w=1280");
  });

  it("returns nothing for a slide without artwork", () => {
    expect(heroBackdropUrl(slide("a"), false)).toBeUndefined();
    expect(heroBackdropUrl(undefined, true)).toBeUndefined();
  });
});

describe("heroOrientation", () => {
  it("is landscape for wide boxes and portrait for tall ones", () => {
    expect(heroOrientation(1440, 820)).toBe("landscape");
    expect(heroOrientation(390, 620)).toBe("portrait");
    expect(heroOrientation(820, 680)).toBe("landscape");
    expect(heroOrientation(700, 900)).toBe("portrait");
  });

  it("falls back to landscape when the box has no size", () => {
    expect(heroOrientation(0, 0)).toBe("landscape");
    expect(heroOrientation(390, 0)).toBe("landscape");
  });

  it("keeps the previous orientation inside the hysteresis band", () => {
    expect(heroOrientation(1.12 * 600, 600, "portrait")).toBe("portrait");
    expect(heroOrientation(1.08 * 600, 600, "landscape")).toBe("landscape");
    expect(heroOrientation(1.2 * 600, 600, "portrait")).toBe("landscape");
    expect(heroOrientation(1.0 * 600, 600, "landscape")).toBe("portrait");
  });
});

describe("heroBackdropUrl by orientation", () => {
  it("prefers the poster in a portrait box, then fanart, then thumb", () => {
    expect(heroBackdropUrl(slide("a", { fanart: "fa", poster: "po" }), false, "portrait"))
      .toBe("/api/v1/items/a/images/poster?tag=po");
    expect(heroBackdropUrl(slide("a", { fanart: "fa" }), false, "portrait"))
      .toBe("/api/v1/items/a/images/fanart?tag=fa");
    expect(heroBackdropUrl(slide("a", { thumb: "th" }), false, "portrait"))
      .toBe("/api/v1/items/a/images/thumb?tag=th");
  });

  it("keeps fanart first in a landscape box", () => {
    expect(heroBackdropUrl(slide("a", { fanart: "fa", poster: "po" }), false, "landscape"))
      .toBe("/api/v1/items/a/images/fanart?tag=fa");
  });

  it("uses the poster item id of a season in a portrait box", () => {
    const season: MediaItem = { id: "s1", title: "s", itemType: "SEASON", posterItemId: "show", imageTags: { poster: "p" } };
    expect(heroBackdropUrl(season, false, "portrait")).toBe("/api/v1/items/show/images/poster?tag=p");
  });
});

describe("nextHeroBackdropUrl", () => {
  const slides = [slide("a", { fanart: "fa" }), slide("b", { fanart: "fb" }), slide("c", { poster: "pc" })];

  it("returns the artwork of the following slide", () => {
    expect(nextHeroBackdropUrl(slides, 0, false)).toBe("/api/v1/items/b/images/fanart?tag=fb");
  });

  it("wraps around to the first slide after the last one", () => {
    expect(nextHeroBackdropUrl(slides, 2, false)).toBe("/api/v1/items/a/images/fanart?tag=fa");
  });

  it("uses the same fanart-then-poster rule as the visible backdrop", () => {
    expect(nextHeroBackdropUrl(slides, 1, true)).toBe(heroBackdropUrl(slides[2], true));
  });

  it("follows the orientation of the visible backdrop", () => {
    const both = [slide("a", { fanart: "fa", poster: "pa" }), slide("b", { fanart: "fb", poster: "pb" })];
    expect(nextHeroBackdropUrl(both, 0, false, "portrait")).toBe("/api/v1/items/b/images/poster?tag=pb");
    expect(nextHeroBackdropUrl(both, 0, false, "landscape")).toBe("/api/v1/items/b/images/fanart?tag=fb");
  });

  it("does not preload when there is no next slide", () => {
    expect(nextHeroBackdropUrl([], 0, false)).toBeUndefined();
    expect(nextHeroBackdropUrl([slide("a", { fanart: "fa" })], 0, false)).toBeUndefined();
  });

  it("does not preload a next slide without artwork", () => {
    expect(nextHeroBackdropUrl([slide("a", { fanart: "fa" }), slide("b")], 0, false)).toBeUndefined();
  });
});

describe("preloadHeroBackdrop", () => {
  beforeEach(() => {
    FakeImage.instances = [];
    FakeImage.decodeResult = "resolve";
    vi.stubGlobal("Image", FakeImage);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("loads the url without spawning a visible element", async () => {
    await expect(preloadHeroBackdrop("/api/v1/items/b/images/fanart?tag=fb")).resolves.toBe(true);
    expect(FakeImage.instances).toHaveLength(1);
    expect(FakeImage.instances[0].src).toBe("/api/v1/items/b/images/fanart?tag=fb");
    expect(document.querySelector("img")).toBeNull();
  });

  it("reports failures instead of throwing when decoding fails", async () => {
    FakeImage.decodeResult = "reject";
    await expect(preloadHeroBackdrop("/api/v1/items/b/images/fanart?tag=fb")).resolves.toBe(false);
  });

  it("ignores a preload that was cancelled before it started", async () => {
    const controller = new AbortController();
    controller.abort();
    await expect(preloadHeroBackdrop("/api/v1/items/b/images/fanart?tag=fb", controller.signal))
      .resolves.toBe(false);
    expect(FakeImage.instances).toHaveLength(0);
  });
});

describe("useHeroBackdropPreload", () => {
  type PreloadProbeProps = {
    items: MediaItem[];
    activeIndex: number;
    variantsEnabled?: boolean;
    orientation?: "landscape" | "portrait" | undefined;
  };

  let container: HTMLDivElement | undefined;
  let root: Root | undefined;

  function PreloadProbe(props: PreloadProbeProps) {
    const { items, activeIndex, variantsEnabled = false } = props;
    const orientation = "orientation" in props ? props.orientation : "landscape";
    useHeroBackdropPreload({ items, activeIndex, variantsEnabled, orientation });
    return null;
  }

  beforeEach(() => {
    FakeImage.instances = [];
    FakeImage.decodeResult = "resolve";
    vi.stubGlobal("Image", FakeImage);
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
    root = undefined;
    container = undefined;
    vi.unstubAllGlobals();
  });

  it("warms exactly the next slide and nothing beyond it", () => {
    const slides = [slide("a", { fanart: "fa" }), slide("b", { fanart: "fb" }), slide("c", { fanart: "fc" })];
    act(() => root?.render(<PreloadProbe items={slides} activeIndex={0} />));
    expect(FakeImage.instances.map((image) => image.src)).toEqual(["/api/v1/items/b/images/fanart?tag=fb"]);

    act(() => root?.render(<PreloadProbe items={slides} activeIndex={1} />));
    expect(FakeImage.instances.map((image) => image.src)).toEqual([
      "/api/v1/items/b/images/fanart?tag=fb",
      "/api/v1/items/c/images/fanart?tag=fc",
    ]);
  });

  it("warms the poster of the next slide in a portrait box", () => {
    const slides = [slide("a", { fanart: "fa", poster: "pa" }), slide("b", { fanart: "fb", poster: "pb" })];
    act(() => root?.render(<PreloadProbe items={slides} activeIndex={0} orientation="portrait" />));
    expect(FakeImage.instances.map((image) => image.src)).toEqual(["/api/v1/items/b/images/poster?tag=pb"]);
  });

  it("waits for the box to be measured before preloading", () => {
    const slides = [slide("a", { fanart: "fa" }), slide("b", { fanart: "fb" })];
    act(() => root?.render(<PreloadProbe items={slides} activeIndex={0} orientation={undefined} />));
    expect(FakeImage.instances).toHaveLength(0);
  });

  it("skips preloading when a single slide cannot rotate", () => {
    act(() => root?.render(<PreloadProbe items={[slide("a", { fanart: "fa" })]} activeIndex={0} />));
    expect(FakeImage.instances).toHaveLength(0);
  });

  it("does not preload while the hero is still waiting for its first slides", () => {
    act(() => root?.render(<PreloadProbe items={[]} activeIndex={0} />));
    expect(FakeImage.instances).toHaveLength(0);
  });
});

describe("useHeroOrientation", () => {
  type Size = { width: number; height: number };
  let size: Size;
  let observerCallback: (() => void) | undefined;
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;
  let seen: Array<"landscape" | "portrait" | undefined>;

  class FakeResizeObserver {
    constructor(callback: () => void) {
      observerCallback = callback;
    }
    observe() {}
    disconnect() {
      observerCallback = undefined;
    }
  }

  function Probe() {
    const ref = useRef<HTMLDivElement>(null);
    const orientation = useHeroOrientation(ref);
    seen.push(orientation);
    return <div ref={ref} data-orientation={orientation ?? "unknown"} />;
  }

  beforeEach(() => {
    size = { width: 390, height: 620 };
    seen = [];
    observerCallback = undefined;
    vi.stubGlobal("ResizeObserver", FakeResizeObserver);
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(
      () => ({ ...size, x: 0, y: 0, top: 0, left: 0, right: size.width, bottom: size.height, toJSON: () => ({}) }),
    );
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
    root = undefined;
    container = undefined;
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it("measures the box before the first paint so no wrong image is requested", () => {
    act(() => root?.render(<Probe />));
    expect(container?.querySelector("div")?.getAttribute("data-orientation")).toBe("portrait");
  });

  it("re-evaluates when the box is resized, as on a tablet rotation", () => {
    size = { width: 1180, height: 700 };
    act(() => root?.render(<Probe />));
    expect(container?.querySelector("div")?.getAttribute("data-orientation")).toBe("landscape");
    size = { width: 700, height: 900 };
    act(() => observerCallback?.());
    expect(container?.querySelector("div")?.getAttribute("data-orientation")).toBe("portrait");
  });

  it("treats an unmeasurable box as landscape", () => {
    size = { width: 0, height: 0 };
    act(() => root?.render(<Probe />));
    expect(container?.querySelector("div")?.getAttribute("data-orientation")).toBe("landscape");
  });
});
