// @vitest-environment jsdom

import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MediaCardVisual } from "../src/features/home/media-card-visual";
import { ImageVariantsContext } from "../src/features/home/image-variants";
import { MediaStills } from "../src/features/detail/MediaStills";
import type { ItemImage, MediaItem } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const item = {
  id: "movie-1",
  itemType: "MOVIE",
  title: "优化图片测试",
  imageTags: { poster: "poster-tag", fanart: "fanart-tag" },
} as MediaItem;

describe("image variant rendering", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  function render(children: ReactNode, enabled: boolean) {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    act(() => {
      root.render(
        <MemoryRouter>
          <ImageVariantsContext.Provider value={{ available: enabled, enabled }}>
            {children}
          </ImageVariantsContext.Provider>
        </MemoryRouter>,
      );
    });
  }

  it("requests a 360px card variant only when enabled", () => {
    render(<MediaCardVisual item={item} landscape={false} />, true);
    expect(container.querySelector("img")?.getAttribute("src")).toContain("poster-tag&w=360");
    act(() => root.unmount());
    container.remove();

    render(<MediaCardVisual item={item} landscape={false} />, false);
    expect(container.querySelector("img")?.getAttribute("src")).toBe(
      "/api/v1/items/movie-1/images/poster?tag=poster-tag",
    );
  });

  it("requests a 720px still variant only when enabled", () => {
    const images: ItemImage[] = [
      {
        id: "image-1",
        itemId: "movie-1",
        imageType: "FANART",
        imageIndex: 0,
        url: "/api/v1/items/movie-1/images/fanart/0?tag=still-tag",
      } as ItemImage,
    ];
    render(<MediaStills images={images} title="优化图片测试" />, true);
    expect(container.querySelector("img")?.getAttribute("src")).toBe(
      "/api/v1/items/movie-1/images/fanart/0?tag=still-tag&w=720",
    );
  });
});
