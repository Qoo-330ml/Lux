// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { CollectionDetailPage } from "../src/features/detail/CollectionDetailPage";
import { api } from "../src/lib/api/client";
import type { CollectionDetailResponse } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("CollectionDetailPage", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
    vi.restoreAllMocks();
  });

  async function renderCollection(response: CollectionDetailResponse) {
    vi.spyOn(api, "collection").mockResolvedValue(response);
    const rendered = document.createElement("div");
    container = rendered;
    document.body.append(rendered);
    root = createRoot(rendered);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/collections/collection-1"]}>
            <Routes>
              <Route path="/collections/:collectionId" element={<CollectionDetailPage />} />
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await vi.waitFor(() => expect(container?.querySelector(".lux-collection-detail")).not.toBeNull());
    });
    return rendered;
  }

  it("saves an accessible copy and version before linking to the original movie", async () => {
    const response: CollectionDetailResponse = {
      collection: {
        id: "collection-1",
        title: "飞驰人生",
        itemType: "BOX_SET",
        productionYear: 2019,
        memberCount: 2,
        posterItemId: "movie-1",
        imageTags: { poster: "collection-poster" },
      },
      items: [{
        id: "work-1",
        title: "飞驰人生",
        itemType: "MOVIE",
        productionYear: 2019,
        preferredSourceItemId: "movie-1",
        preferredMediaSourceId: "version-1",
        sources: [
          {
            itemId: "movie-1",
            libraryId: "library-1",
            libraryName: "客厅电影",
            title: "飞驰人生",
            posterImageTag: "movie-poster-1",
            versions: [{ id: "version-1", editionName: "影院版", qualityLabel: "1080p", container: "mkv" }],
          },
          {
            itemId: "movie-2",
            libraryId: "library-2",
            libraryName: "收藏电影",
            title: "飞驰人生 4K",
            posterImageTag: "movie-poster-2",
            versions: [{ id: "version-2", editionName: "4K 修复版", qualityLabel: "2160p", container: "mkv" }],
          },
        ],
      }],
      page: 1,
      pageSize: 24,
      total: 1,
    };
    const savePreference = vi.spyOn(api, "setCollectionSourcePreference").mockResolvedValue(undefined);
    const rendered = await renderCollection(response);

    const sourceSelect = rendered.querySelector<HTMLSelectElement>("select[aria-label='选择《飞驰人生》的可访问来源']");
    expect(sourceSelect?.options).toHaveLength(2);
    await act(async () => {
      if (!sourceSelect) throw new Error("missing collection source selector");
      sourceSelect.value = sourceSelect.options[1].value;
      sourceSelect.dispatchEvent(new Event("change", { bubbles: true }));
    });
    await vi.waitFor(() => expect(savePreference).toHaveBeenCalledWith("collection-1", "work-1", {
      sourceItemId: "movie-2",
      mediaSourceId: "version-2",
    }));

    expect(rendered.querySelector<HTMLAnchorElement>("[data-action='open-selected-movie']")?.getAttribute("href"))
      .toBe("/items/movie-2?collectionId=collection-1&sourceId=version-2");
    expect(rendered.textContent).toContain("按影片上映顺序排列");
  });

  it("shows a useful empty state for a collection with no visible movies", async () => {
    const rendered = await renderCollection({
      collection: { id: "collection-1", title: "飞驰人生", itemType: "BOX_SET", memberCount: 0 },
      items: [],
      page: 1,
      pageSize: 24,
      total: 0,
    });

    expect(rendered.textContent).toContain("这个系列目前没有可见影片。");
    expect(rendered.querySelector("a[href='/libraries/00000000-0000-7000-8000-000000000538']")).not.toBeNull();
  });
});
