// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider, notifyManager } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { MediaDetailPage } from "../src/features/detail/MediaDetailPage";
import { api, ApiError } from "../src/lib/api/client";
import type { ItemImage, MediaItem, PageResponse } from "../src/lib/api/types";
import { mockPlaybackBootstrap } from "./player-test-helpers";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const SIMILAR_LIMIT = 12;

type SimilarResult = PageResponse<MediaItem> | Error | Promise<PageResponse<MediaItem>>;

function similarItems(count: number, idPrefix = "similar"): MediaItem[] {
  return Array.from({ length: count }, (_, index) => ({
    id: `${idPrefix}-${index + 1}`,
    title: `相似条目 ${index + 1}`,
    itemType: index % 2 === 0 ? "MOVIE" : "SERIES",
    productionYear: 2000 + index,
  }));
}

function detailItem(id: string, itemType: string): MediaItem {
  return {
    id,
    title: "当前条目",
    itemType,
    mediaSources: [{ id: "source-1", isDefault: true, container: "mkv" }],
  };
}

describe("MediaSimilarSection", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;

  beforeEach(() => {
    notifyManager.setScheduler(queueMicrotask);
    mockPlaybackBootstrap();
    vi.spyOn(api, "itemImages").mockResolvedValue({ images: [] });
    vi.spyOn(api, "playback").mockResolvedValue({});
    vi.spyOn(api, "children").mockResolvedValue({ items: [], total: 0, page: 1, pageSize: 100 });
    vi.spyOn(api, "me").mockResolvedValue({
      user: { id: "viewer-1", usernameNormalized: "viewer", displayName: "Viewer", canManageServer: false },
    });
  });

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
    vi.restoreAllMocks();
    notifyManager.setScheduler((callback) => setTimeout(callback, 0));
  });

  async function renderDetail(
    item: MediaItem,
    {
      images = [],
      similar,
    }: { images?: ItemImage[]; similar?: SimilarResult } = {},
  ) {
    if (root) act(() => root?.unmount());
    container?.remove();
    vi.spyOn(api, "item").mockResolvedValue(item);
    vi.mocked(api.itemImages).mockResolvedValue({ images });
    const similarSpy = vi.spyOn(api, "similarItems").mockImplementation(() => {
      if (similar instanceof Error) return Promise.reject(similar);
      if (similar instanceof Promise) return similar;
      return Promise.resolve(similar ?? { items: [] });
    });

    const rendered = document.createElement("div");
    container = rendered;
    document.body.append(rendered);
    root = createRoot(rendered);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={[`/items/${item.id}`]}>
            <Routes>
              <Route path="items/:itemId" element={<MediaDetailPage />} />
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    return { rendered, similarSpy };
  }

  it("shows at most 12 similar cards after the main content and before stills", async () => {
    const itemId = "movie-1";
    const image: ItemImage = {
      id: "fanart-1",
      itemId,
      imageType: "FANART",
      imageIndex: 0,
      url: `/api/v1/items/${itemId}/images/fanart-1`,
    };
    const { rendered, similarSpy } = await renderDetail(detailItem(itemId, "MOVIE"), {
      images: [image],
      similar: { items: similarItems(13), page: 1, pageSize: SIMILAR_LIMIT, total: 13 },
    });

    expect(similarSpy).toHaveBeenCalledTimes(1);
    expect(similarSpy).toHaveBeenCalledWith(itemId, { limit: SIMILAR_LIMIT });

    const section = rendered.querySelector(".lux-detail-similar");
    expect(section).not.toBeNull();
    expect(section?.querySelector(".lux-section-heading h2")?.textContent).toBe("相似推荐");
    expect(section?.getAttribute("aria-labelledby")).toBe("media-similar-heading");
    expect(section?.querySelector(".lux-section-heading h2")?.getAttribute("id")).toBe("media-similar-heading");

    const cards = section?.querySelectorAll(".lux-media-card") ?? [];
    expect(cards).toHaveLength(SIMILAR_LIMIT);
    expect(cards[0]?.querySelector<HTMLAnchorElement>(".lux-media-card-link")?.getAttribute("href"))
      .toBe("/items/similar-1");
    expect(cards[1]?.querySelector<HTMLAnchorElement>(".lux-media-card-link")?.getAttribute("href"))
      .toBe("/items/similar-2");

    const cast = rendered.querySelector(".lux-detail-cast");
    const stills = rendered.querySelector(".lux-detail-stills");
    expect(cast && section && (cast.compareDocumentPosition(section) & Node.DOCUMENT_POSITION_FOLLOWING)).toBeTruthy();
    expect(section && stills && (section.compareDocumentPosition(stills) & Node.DOCUMENT_POSITION_FOLLOWING)).toBeTruthy();
  });

  it("shows similar items on a series detail", async () => {
    const { rendered, similarSpy } = await renderDetail(detailItem("series-1", "SERIES"), {
      similar: { items: similarItems(2, "series-similar") },
    });

    expect(similarSpy).toHaveBeenCalledWith("series-1", { limit: SIMILAR_LIMIT });
    expect(rendered.querySelectorAll(".lux-detail-similar .lux-media-card")).toHaveLength(2);
    expect(rendered.querySelector(".lux-detail-similar .lux-media-card-link")?.getAttribute("href"))
      .toBe("/items/series-similar-1");
  });

  it("does not request or render for seasons, episodes, collections, or videos", async () => {
    for (const itemType of ["SEASON", "EPISODE", "BOX_SET", "VIDEO"]) {
      const { rendered, similarSpy } = await renderDetail(detailItem(`excluded-${itemType.toLowerCase()}`, itemType), {
        similar: { items: similarItems(2) },
      });

      expect(similarSpy, itemType).not.toHaveBeenCalled();
      expect(rendered.querySelector(".lux-detail-similar"), itemType).toBeNull();
      expect(rendered.querySelector(".lux-detail-page"), itemType).not.toBeNull();
    }
  });

  it("renders no section when the similar result is empty", async () => {
    const { rendered } = await renderDetail(detailItem("movie-empty", "MOVIE"), {
      similar: { items: [], page: 1, pageSize: SIMILAR_LIMIT, total: 0 },
    });

    expect(rendered.querySelector(".lux-detail-similar")).toBeNull();
    expect(rendered.querySelector(".lux-detail-title-row h1")?.textContent).toBe("当前条目");
  });

  it("renders no section when the similar request fails with 401, 404, or 503", async () => {
    for (const status of [401, 404, 503]) {
      const { rendered } = await renderDetail(detailItem(`movie-error-${status}`, "MOVIE"), {
        similar: new ApiError("相似推荐不可用", { code: "SIMILAR_UNAVAILABLE", status }),
      });

      expect(rendered.querySelector(".lux-detail-similar"), String(status)).toBeNull();
      expect(rendered.querySelector(".lux-detail-title-row h1"), String(status)).not.toBeNull();
    }
  });

  it("renders no loading placeholder while the similar request is pending", async () => {
    let resolveSimilar: ((value: PageResponse<MediaItem>) => void) | undefined;
    const pending = new Promise<PageResponse<MediaItem>>((resolve) => {
      resolveSimilar = resolve;
    });
    const { rendered, similarSpy } = await renderDetail(detailItem("movie-pending", "MOVIE"), {
      similar: pending,
    });

    expect(similarSpy).toHaveBeenCalledTimes(1);
    expect(rendered.querySelector(".lux-detail-similar")).toBeNull();
    expect(rendered.querySelector(".lux-detail-title-row h1")?.textContent).toBe("当前条目");

    await act(async () => {
      resolveSimilar?.({ items: similarItems(1) });
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(rendered.querySelectorAll(".lux-detail-similar .lux-media-card")).toHaveLength(1);
  });
});

describe("api.similarItems", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("requests only limit=12 without page or pageSize", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ items: [], page: 1, pageSize: SIMILAR_LIMIT, total: 0 }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      }),
    );

    await expect(api.similarItems("movie-1", { limit: SIMILAR_LIMIT })).resolves.toEqual({
      items: [],
      page: 1,
      pageSize: SIMILAR_LIMIT,
      total: 0,
    });

    const requested = String(fetchMock.mock.calls[0]?.[0]);
    expect(requested).toBe("/api/v1/items/movie-1/similar?limit=12");
    const params = new URL(requested, "http://lux.invalid").searchParams;
    expect([...params.keys()]).toEqual(["limit"]);
    expect(params.get("limit")).toBe("12");
  });

  it("defaults to limit=12 and encodes the item id", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ items: [] }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      }),
    );

    await api.similarItems("item/with space");

    expect(String(fetchMock.mock.calls[0]?.[0])).toBe("/api/v1/items/item%2Fwith%20space/similar?limit=12");
  });
});
