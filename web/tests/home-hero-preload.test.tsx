// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { MediaItem } from "../src/lib/api/types";
import {
  heroBackdropUrl,
  nextHeroBackdropUrl,
  preloadHeroBackdrop,
  useHeroBackdropPreload,
} from "../src/features/home/hero-preload";

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
  };

  let container: HTMLDivElement | undefined;
  let root: Root | undefined;

  function PreloadProbe({ items, activeIndex, variantsEnabled = false }: PreloadProbeProps) {
    useHeroBackdropPreload({ items, activeIndex, variantsEnabled });
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

  it("skips preloading when a single slide cannot rotate", () => {
    act(() => root?.render(<PreloadProbe items={[slide("a", { fanart: "fa" })]} activeIndex={0} />));
    expect(FakeImage.instances).toHaveLength(0);
  });

  it("does not preload while the hero is still waiting for its first slides", () => {
    act(() => root?.render(<PreloadProbe items={[]} activeIndex={0} />));
    expect(FakeImage.instances).toHaveLength(0);
  });
});
