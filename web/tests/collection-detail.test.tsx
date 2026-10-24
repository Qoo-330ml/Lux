// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import { CollectionDetailPage } from "../src/features/detail/CollectionDetailPage";
import { api } from "../src/lib/api/client";
import type { CollectionDetailResponse } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
// jsdom does not implement native dialog methods; browser checks cover modal focus behavior.
HTMLDialogElement.prototype.showModal = function () { this.open = true; };
HTMLDialogElement.prototype.close = function () { this.open = false; };

function MovieDestination() {
  const location = useLocation();
  return <p>已进入原资源详情 {location.pathname}{location.search}</p>;
}

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
              <Route path="/items/:itemId" element={<MovieDestination />} />
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

  it.each(["poster", "title"])("opens source choices from the %s and enters the chosen original movie after saving", async (target) => {
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

    expect(rendered.querySelector("select, dialog[open]")).toBeNull();
    expect(rendered.querySelector(".lux-collection-work")?.textContent).not.toContain("来源与版本");
    await act(async () => {
      const card = rendered.querySelector(".lux-collection-work");
      const trigger = card?.querySelectorAll<HTMLButtonElement>("button.lux-media-card-link")[target === "poster" ? 0 : 1];
      if (!trigger) throw new Error("missing source dialog trigger");
      trigger.click();
    });
    const dialog = rendered.querySelector("dialog[open]");
    expect(dialog?.textContent).toContain("选择影片来源");
    expect(dialog?.textContent).toContain("客厅电影");
    expect(dialog?.textContent).toContain("收藏电影");
    expect(savePreference).not.toHaveBeenCalled();
    await act(async () => {
      const choices = dialog?.querySelectorAll<HTMLButtonElement>(".lux-collection-source-option");
      if (!choices?.[1]) throw new Error("missing second source choice");
      choices[1].click();
    });
    await vi.waitFor(() => expect(savePreference).toHaveBeenCalledWith("collection-1", "work-1", {
      sourceItemId: "movie-2",
      mediaSourceId: "version-2",
    }));

    await vi.waitFor(() => expect(rendered.textContent).toContain("已进入原资源详情"));
    expect(rendered.textContent).toContain("/items/movie-2?collectionId=collection-1&sourceId=version-2");
  });

  it("enters a single source directly even when it has multiple versions", async () => {
    const savePreference = vi.spyOn(api, "setCollectionSourcePreference").mockResolvedValue(undefined);
    const rendered = await renderCollection({
      collection: { id: "collection-1", title: "飞驰人生", itemType: "BOX_SET" },
      items: [{
        id: "work-1", title: "飞驰人生", preferredSourceItemId: "movie-1", preferredMediaSourceId: "4k",
        sources: [{ itemId: "movie-1", libraryId: "movies", libraryName: "电影", title: "飞驰人生", versions: [{ id: "hd" }, { id: "4k" }] }],
      }],
      page: 1, pageSize: 24, total: 1,
    });
    const link = rendered.querySelector<HTMLAnchorElement>(".lux-media-card-link");
    expect(rendered.querySelector("select")).toBeNull();
    expect(link?.getAttribute("href")).toBe("/items/movie-1?collectionId=collection-1&sourceId=4k");
    await act(async () => link?.click());
    expect(rendered.textContent).toContain("已进入原资源详情");
    expect(rendered.querySelector("dialog")).toBeNull();
    expect(savePreference).not.toHaveBeenCalled();
  });

  it("keeps the picker open after a save failure and allows retry", async () => {
    const savePreference = vi.spyOn(api, "setCollectionSourcePreference")
      .mockRejectedValueOnce(new Error("稍后重试"))
      .mockResolvedValue(undefined);
    const rendered = await renderCollection({
      collection: { id: "collection-1", title: "飞驰人生", itemType: "BOX_SET" },
      items: [{ id: "work-1", title: "飞驰人生", sources: [1, 2].map((number) => ({
        itemId: `movie-${number}`, libraryId: `library-${number}`, libraryName: `电影库 ${number}`, title: "飞驰人生", versions: [],
      })) }],
      page: 1, pageSize: 24, total: 1,
    });
    await act(async () => rendered.querySelector<HTMLButtonElement>("button.lux-media-card-link")?.click());
    await act(async () => {
      rendered.querySelector<HTMLButtonElement>(".lux-collection-source-option")?.click();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    await vi.waitFor(() => expect(rendered.querySelector("dialog[open] [role='alert']")?.textContent).toContain("来源保存失败"));
    expect(rendered.textContent).not.toContain("已进入原资源详情");
    await act(async () => rendered.querySelector<HTMLButtonElement>(".lux-collection-source-option")?.click());
    await vi.waitFor(() => expect(rendered.textContent).toContain("已进入原资源详情"));
    expect(savePreference).toHaveBeenCalledTimes(2);
  });

  it("closes source choices without saving or navigating", async () => {
    const savePreference = vi.spyOn(api, "setCollectionSourcePreference").mockResolvedValue(undefined);
    const rendered = await renderCollection({
      collection: { id: "collection-1", title: "飞驰人生", itemType: "BOX_SET" },
      items: [{ id: "work-1", title: "飞驰人生", sources: [1, 2].map((number) => ({
        itemId: `movie-${number}`, libraryId: `library-${number}`, libraryName: `电影库 ${number}`, title: "飞驰人生", versions: [],
      })) }],
      page: 1, pageSize: 24, total: 1,
    });
    await act(async () => rendered.querySelector<HTMLButtonElement>("button.lux-media-card-link")?.click());
    expect(rendered.querySelector("dialog[open]")).not.toBeNull();
    await act(async () => rendered.querySelector<HTMLButtonElement>("button[aria-label='关闭来源选择']")?.click());
    expect(rendered.querySelector("dialog")).toBeNull();
    expect(savePreference).not.toHaveBeenCalled();
    expect(rendered.querySelector(".lux-collection-detail")).not.toBeNull();
  });

  it("does not navigate after closing a picker with a pending save", async () => {
    let finishSave = () => {};
    const saving = new Promise<void>((resolve) => { finishSave = resolve; });
    const savePreference = vi.spyOn(api, "setCollectionSourcePreference").mockReturnValue(saving);
    const rendered = await renderCollection({
      collection: { id: "collection-1", title: "飞驰人生", itemType: "BOX_SET" },
      items: [{ id: "work-1", title: "飞驰人生", sources: [1, 2].map((number) => ({
        itemId: `movie-${number}`, libraryId: `library-${number}`, libraryName: `电影库 ${number}`, title: "飞驰人生", versions: [],
      })) }],
      page: 1, pageSize: 24, total: 1,
    });
    await act(async () => rendered.querySelector<HTMLButtonElement>("button.lux-media-card-link")?.click());
    await act(async () => rendered.querySelector<HTMLButtonElement>(".lux-collection-source-option")?.click());
    expect(savePreference).toHaveBeenCalledTimes(1);
    await act(async () => rendered.querySelector<HTMLButtonElement>("button[aria-label='关闭来源选择']")?.click());
    await act(async () => { finishSave(); await saving; });
    expect(rendered.querySelector("dialog")).toBeNull();
    expect(rendered.querySelector(".lux-collection-detail")).not.toBeNull();
    expect(rendered.textContent).not.toContain("已进入原资源详情");
  });

  it("shows collection identity and overview above a rail of original movie cards", async () => {
    const rendered = await renderCollection({
      collection: {
        id: "collection-1", title: "飞驰人生（系列）", originalTitle: "Pegasus Collection", itemType: "BOX_SET",
        overview: "一群热爱赛车的朋友，在赛道上追逐梦想的故事。", productionYear: 2019,
        posterItemId: "movie-1", imageTags: { poster: "poster", fanart: "backdrop" },
      },
      items: [1, 2].map((number) => ({
        id: `work-${number}`, title: `飞驰人生 ${number}`, itemType: "MOVIE", productionYear: 2019 + number,
        sources: [{
          itemId: `movie-${number}`, libraryId: "movies", libraryName: "电影", title: `飞驰人生 ${number}`,
          posterImageTag: `poster-${number}`, versions: [{ id: `version-${number}`, qualityLabel: "1080p" }],
        }],
      })),
      page: 1, pageSize: 24, total: 2,
    });

    expect(rendered.querySelector(".lux-detail-page .lux-detail-grid h1")?.textContent).toBe("飞驰人生（系列）");
    expect(rendered.querySelector(".lux-detail-original-title")?.textContent).toBe("Pegasus Collection");
    expect(rendered.querySelector(".lux-detail-overview")?.textContent).toContain("在赛道上追逐梦想");
    expect(rendered.querySelector(".lux-detail-poster img")?.getAttribute("src")).toBe("/api/v1/items/movie-1/images/poster?tag=poster");
    expect(rendered.querySelector(".lux-detail-backdrop")?.getAttribute("src")).toBe("/api/v1/items/collection-1/images/fanart?tag=backdrop");
    const rail = rendered.querySelector(".lux-horizontal-scroll-viewport .lux-media-rail");
    expect(rail?.querySelectorAll(".lux-media-card")).toHaveLength(2);
    expect(Array.from(rail?.querySelectorAll(".lux-media-copy strong") ?? [], (node) => node.textContent))
      .toEqual(["飞驰人生 1", "飞驰人生 2"]);
    expect(rail?.querySelector("a")?.getAttribute("href")).toBe("/items/movie-1?collectionId=collection-1&sourceId=version-1");
    expect(rendered.querySelector("select")).toBeNull();
    expect(rendered.textContent).not.toContain("打开影片详情");
  });

  it("keeps unavailable members unlinked and provides an overview fallback", async () => {
    const rendered = await renderCollection({
      collection: { id: "collection-1", title: "飞驰人生", itemType: "BOX_SET" },
      items: [{ id: "work-1", title: "飞驰人生", sources: [] }],
      page: 1, pageSize: 24, total: 1,
    });

    expect(rendered.querySelector(".lux-detail-overview")?.textContent).toContain("暂无简介。");
    const card = rendered.querySelector(".lux-media-card");
    expect(card?.textContent).toContain("该影片目前没有可访问的播放副本。");
    expect(card?.querySelector("a")).toBeNull();
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
