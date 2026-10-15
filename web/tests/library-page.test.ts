// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { LibraryPage, libraryItemTypeFilter } from "../src/features/library/LibraryPage";
import { useUserEvents } from "../src/features/activity/useUserEvents";
import { api } from "../src/lib/api/client";
import type { LibrariesResponse } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

class FakeUserEventSource {
  static instances: FakeUserEventSource[] = [];
  private listeners = new Map<string, Set<(event: Event) => void>>();

  constructor(readonly url: string) {
    FakeUserEventSource.instances.push(this);
  }

  addEventListener(type: string, listener: EventListenerOrEventListenerObject | null) {
    if (!listener) return;
    const handlers = this.listeners.get(type) ?? new Set<(event: Event) => void>();
    handlers.add(typeof listener === "function" ? listener : (event) => listener.handleEvent(event));
    this.listeners.set(type, handlers);
  }

  removeEventListener(type: string, listener: EventListenerOrEventListenerObject | null) {
    if (!listener) return;
    const handlers = this.listeners.get(type);
    if (!handlers) return;
    const handler = typeof listener === "function" ? listener : (event: Event) => listener.handleEvent(event);
    handlers.delete(handler);
  }

  close() {}

  emit(type: string, data: string) {
    const event = new MessageEvent(type, { data });
    for (const handler of this.listeners.get(type) ?? []) handler(event);
  }
}

function ProgressiveLibraryPage() {
  useUserEvents();
  return createElement(LibraryPage);
}

describe("libraryItemTypeFilter", () => {
  it("shows only series at the root of a series library", () => {
    expect(libraryItemTypeFilter("SERIES")).toBe("SERIES");
  });

  it("shows only movies at the root of a movie library", () => {
    expect(libraryItemTypeFilter("MOVIE")).toBe("MOVIE");
  });

  it("shows both top-level types in a mixed library", () => {
    expect(libraryItemTypeFilter("MIXED")).toBe("MOVIE,SERIES");
  });
});

describe("LibraryPage infinite scroll", () => {
  beforeEach(() => { vi.spyOn(window, "scrollTo").mockImplementation(() => {}); });
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;
  let triggerIntersection: (() => void) | undefined;

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
    document.title = "Lux";
    localStorage.clear();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    triggerIntersection = undefined;
  });

  it("bounds mounted cards when 504 library items are already loaded", async () => {
    vi.spyOn(api, "libraries").mockResolvedValue({
      libraries: [{ id: "large-library", name: "大库", kind: "MOVIE" }],
    });
    vi.spyOn(api, "libraryItems").mockResolvedValue({
      items: Array.from({ length: 504 }, (_, index) => ({
        id: `movie-${index}`, title: `Movie ${index}`, itemType: "MOVIE",
      })),
      total: 504, page: 1, pageSize: 504,
    });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    await act(async () => {
      root?.render(createElement(QueryClientProvider, { client: queryClient },
        createElement(MemoryRouter, { initialEntries: ["/libraries/large-library"] },
          createElement(Routes, null,
            createElement(Route, { path: "/libraries/:libraryId", element: createElement(LibraryPage) })))));
    });
    await act(async () => {
      await vi.waitFor(() => expect(container?.querySelectorAll(".lux-media-card").length).toBeGreaterThan(0));
    });
    expect(container.querySelectorAll(".lux-media-card").length).toBeLessThan(50);
    expect(container.textContent).toContain("已加载全部 504 项");
  });

  it("browses HomeVideos folders and keeps videos as playable media", async () => {
    vi.spyOn(api, "libraries").mockResolvedValue({
      libraries: [{ id: "home-videos", name: "其他视频", kind: "HOMEVIDEOS" }],
    });
    const libraryItems = vi.spyOn(api, "libraryItems").mockImplementation(async (_libraryId, _page, _itemTypes, options) => {
      const parentId = (options as typeof options & { parentId?: string }).parentId;
      const items = parentId === "root"
        ? [
          { id: "folder-trips", title: "Trips", itemType: "FOLDER", parentId: "home-videos" },
          { id: "root-video", title: "家庭聚会", itemType: "VIDEO" },
        ]
        : parentId === "folder-trips"
          ? [
            { id: "folder-2024", title: "2024", itemType: "FOLDER", parentId: "folder-trips" },
            { id: "episode-shaped-video", title: "Show S01E01", itemType: "VIDEO", parentId: "folder-trips" },
          ]
          : [{ id: "movie-shaped-video", title: "Movie (2024)", itemType: "VIDEO", parentId: "folder-2024" }];
      return { items, total: items.length, page: 1, pageSize: 24 };
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(createElement(
        QueryClientProvider,
        { client: queryClient },
        createElement(
          MemoryRouter,
          { initialEntries: ["/libraries/home-videos"] },
          createElement(
            Routes,
            null,
            createElement(Route, { path: "/libraries/:libraryId", element: createElement(LibraryPage) }),
          ),
        ),
      ));
    });
    await act(async () => {
      await vi.waitFor(() => expect(container?.querySelector("a[aria-label='打开文件夹 Trips']")).toBeTruthy());
    });

    expect(libraryItems).toHaveBeenCalledWith("home-videos", 1, undefined, {
      sortBy: "Name",
      sortOrder: "Ascending",
      parentId: "root",
    });
    const tripsLink = container.querySelector<HTMLAnchorElement>("a[aria-label='打开文件夹 Trips']");
    expect(tripsLink?.getAttribute("href")).toContain("folderPath=");
    expect(tripsLink?.getAttribute("href")).not.toBe("/items/folder-trips");
    expect(tripsLink?.closest(".lux-media-card")?.querySelector(".lux-media-actions")).toBeNull();
    expect(tripsLink?.closest(".lux-media-card")?.querySelector(".lux-media-hover-play")).toBeNull();
    expect(container.textContent).toContain("家庭聚会");
    expect(container.textContent).toContain("其他视频");

    await act(async () => tripsLink?.click());
    await act(async () => {
      await vi.waitFor(() => expect(container?.textContent).toContain("Show S01E01"));
    });
    expect(libraryItems).toHaveBeenCalledWith("home-videos", 1, undefined, {
      sortBy: "Name",
      sortOrder: "Ascending",
      parentId: "folder-trips",
    });

    const yearFolder = container.querySelector<HTMLAnchorElement>("a[aria-label='打开文件夹 2024']");
    expect(yearFolder).toBeTruthy();
    await act(async () => yearFolder?.click());
    await act(async () => {
      await vi.waitFor(() => expect(container?.textContent).toContain("Movie (2024)"));
    });
    expect(libraryItems).toHaveBeenCalledWith("home-videos", 1, undefined, {
      sortBy: "Name",
      sortOrder: "Ascending",
      parentId: "folder-2024",
    });

    const backButton = container.querySelector<HTMLButtonElement>("button[aria-label='返回上一级']");
    expect(backButton).toBeTruthy();
    await act(async () => backButton?.click());
    await act(async () => {
      await vi.waitFor(() => expect(container?.textContent).toContain("Show S01E01"));
    });
    const backToRoot = container.querySelector<HTMLButtonElement>("button[aria-label='返回上一级']");
    await act(async () => backToRoot?.click());
    await act(async () => {
      await vi.waitFor(() => expect(container?.querySelector("a[aria-label='打开文件夹 Trips']")).toBeTruthy());
    });
  });

  it("renders a newly indexed item and its poster after a scan invalidation", async () => {
    vi.stubGlobal("EventSource", FakeUserEventSource);
    FakeUserEventSource.instances = [];
    vi.spyOn(api, "libraries").mockResolvedValue({
      libraries: [{ id: "scan-library", name: "扫描中的电影", kind: "MOVIE" }],
    });
    let libraryItemRequestCount = 0;
    const libraryItems = vi.spyOn(api, "libraryItems").mockImplementation(async () => {
      libraryItemRequestCount += 1;
      if (libraryItemRequestCount === 1) {
        return { items: [], page: 1, pageSize: 24, total: 0 };
      }
      return {
        items: [{
          id: "scan-movie-1",
          title: "扫描中出现的电影",
          itemType: "MOVIE",
          imageTags: { poster: "scan-poster-v1" },
        }],
        page: 1,
        pageSize: 24,
        total: 1,
      };
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(createElement(
        QueryClientProvider,
        { client: queryClient },
        createElement(
          MemoryRouter,
          { initialEntries: ["/libraries/scan-library"] },
          createElement(
            Routes,
            null,
            createElement(Route, { path: "/libraries/:libraryId", element: createElement(ProgressiveLibraryPage) }),
          ),
        ),
      ));
    });
    expect(api.libraries).toHaveBeenCalled();
    await act(async () => {
      await vi.waitFor(() => expect(container?.textContent).toContain("0 项内容"));
    });
    expect(container.textContent).not.toContain("扫描中出现的电影");
    expect(FakeUserEventSource.instances).toHaveLength(1);
    expect(FakeUserEventSource.instances[0].url).toBe("/api/v1/events");

    await act(async () => {
      FakeUserEventSource.instances[0].emit("invalidate", JSON.stringify({ scope: "home" }));
      await vi.waitFor(() => expect(container?.textContent).toContain("扫描中出现的电影"));
    });

    expect(libraryItems).toHaveBeenCalledTimes(2);
    expect(container.querySelector<HTMLImageElement>(".lux-media-card img")?.getAttribute("src"))
      .toBe("/api/v1/items/scan-movie-1/images/poster?tag=scan-poster-v1");
  });

  it("shows the library name in the browser tab title", async () => {
    vi.spyOn(api, "libraries").mockResolvedValue({
      libraries: [{ id: "library-1", name: "电影收藏", kind: "MOVIE" }],
    });
    vi.spyOn(api, "libraryItems").mockResolvedValue({
      items: [],
      page: 1,
      pageSize: 24,
      total: 0,
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(createElement(
        QueryClientProvider,
        { client: queryClient },
        createElement(
          MemoryRouter,
          { initialEntries: ["/libraries/library-1"] },
          createElement(
            Routes,
            null,
            createElement(Route, { path: "/libraries/:libraryId", element: createElement(LibraryPage, { serverName: "客厅 Lux" }) }),
          ),
        ),
      ));
    });

    await act(async () => {
      await vi.waitFor(() => expect(document.title).toBe("电影收藏 - Lux"));
    });

    act(() => {
      root?.unmount();
      root = undefined;
    });
    expect(document.title).toBe("客厅 Lux - Lux");
  });

  it("keeps the server title while the library name is loading", async () => {
    let resolveLibraries: (response: LibrariesResponse) => void = () => {};
    vi.spyOn(api, "libraries").mockImplementation(() => new Promise((resolve) => {
      resolveLibraries = resolve;
    }));
    vi.spyOn(api, "libraryItems").mockResolvedValue({
      items: [],
      page: 1,
      pageSize: 24,
      total: 0,
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(createElement(
        QueryClientProvider,
        { client: queryClient },
        createElement(
          MemoryRouter,
          { initialEntries: ["/libraries/library-1"] },
          createElement(
            Routes,
            null,
            createElement(Route, { path: "/libraries/:libraryId", element: createElement(LibraryPage, { serverName: "客厅 Lux" }) }),
          ),
        ),
      ));
    });

    expect(document.title).toBe("客厅 Lux - Lux");

    await act(async () => {
      resolveLibraries({ libraries: [{ id: "library-1", name: "电影收藏", kind: "MOVIE" }] });
      await vi.waitFor(() => expect(document.title).toBe("电影收藏 - Lux"));
    });
  });

  it("loads the next library page when the scroll sentinel becomes visible", async () => {
    vi.stubGlobal("IntersectionObserver", class {
      constructor(callback: IntersectionObserverCallback) {
        triggerIntersection = () => callback(
          [{ isIntersecting: true } as IntersectionObserverEntry],
          this as unknown as IntersectionObserver,
        );
      }

      observe() {}

      disconnect() {}
    });

    vi.spyOn(api, "libraries").mockResolvedValue({
      libraries: [{ id: "library-1", name: "电视剧", kind: "SERIES" }],
    });
    const libraryItems = vi.spyOn(api, "libraryItems").mockImplementation(async (_libraryId, page) => ({
      items: [{
        id: `series-${page}`,
        title: `电视剧 ${page}`,
        itemType: "SERIES",
      }],
      page,
      pageSize: 24,
      total: 48,
    }));

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(createElement(
        QueryClientProvider,
        { client: queryClient },
        createElement(
          MemoryRouter,
          { initialEntries: ["/libraries/library-1"] },
          createElement(
            Routes,
            null,
            createElement(Route, { path: "/libraries/:libraryId", element: createElement(LibraryPage) }),
          ),
        ),
      ));
    });
    await act(async () => {
      await vi.waitFor(() => expect(container?.querySelectorAll(".lux-media-card")).toHaveLength(1));
    });

    expect(libraryItems).toHaveBeenCalledWith("library-1", 1, "SERIES", {
      sortBy: "Name",
      sortOrder: "Ascending",
    });
    expect(container.querySelectorAll(".lux-media-card")).toHaveLength(1);

    await act(async () => {
      triggerIntersection?.();
      await vi.waitFor(() => expect(container?.querySelectorAll(".lux-media-card")).toHaveLength(2));
    });

    expect(libraryItems).toHaveBeenCalledWith("library-1", 2, "SERIES", {
      sortBy: "Name",
      sortOrder: "Ascending",
    });
    expect(container.querySelectorAll(".lux-media-card")).toHaveLength(2);
    expect(container.textContent).toContain("电视剧 2");
  });

  it("renders library ratings as compact numeric pills", async () => {
    vi.spyOn(api, "libraries").mockResolvedValue({
      libraries: [{ id: "library-1", name: "电影", kind: "MOVIE" }],
    });
    vi.spyOn(api, "libraryItems").mockResolvedValue({
      items: [{ id: "movie-1", title: "示例电影", itemType: "MOVIE", rating: 6.7, ratingSource: "TMDb" }],
      page: 1,
      pageSize: 24,
      total: 1,
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(createElement(
        QueryClientProvider,
        { client: queryClient },
        createElement(
          MemoryRouter,
          { initialEntries: ["/libraries/library-1"] },
          createElement(
            Routes,
            null,
            createElement(Route, { path: "/libraries/:libraryId", element: createElement(LibraryPage) }),
          ),
        ),
      ));
    });
    await act(async () => {
      await vi.waitFor(() => expect(container?.querySelector(".lux-media-card .lux-rating")).toBeTruthy());
    });

    const badge = container.querySelector(".lux-media-card .lux-rating");
    expect(badge?.classList.contains("lux-rating")).toBe(true);
    expect(badge?.textContent).toBe("6.7");
    expect(badge?.querySelector("svg")).toBeNull();
  });

  it("renders the episode count on series posters", async () => {
    vi.spyOn(api, "libraries").mockResolvedValue({
      libraries: [{ id: "library-1", name: "电视剧", kind: "SERIES" }],
    });
    vi.spyOn(api, "libraryItems").mockResolvedValue({
      items: [{ id: "series-1", title: "示例剧集", itemType: "SERIES", episodeCount: 24 }],
      page: 1,
      pageSize: 24,
      total: 1,
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(createElement(
        QueryClientProvider,
        { client: queryClient },
        createElement(
          MemoryRouter,
          { initialEntries: ["/libraries/library-1"] },
          createElement(
            Routes,
            null,
            createElement(Route, { path: "/libraries/:libraryId", element: createElement(LibraryPage) }),
          ),
        ),
      ));
    });
    await act(async () => {
      await vi.waitFor(() => expect(container?.querySelector(".lux-media-episode-count")).toBeTruthy());
    });

    expect(container.querySelector(".lux-media-episode-count")?.textContent).toBe("24 集");
  });

  it("reloads the first page with the selected release-date sort", async () => {
    vi.spyOn(api, "libraries").mockResolvedValue({
      libraries: [{ id: "library-1", name: "电影", kind: "MOVIE" }],
    });
    const libraryItems = vi.spyOn(api, "libraryItems").mockResolvedValue({
      items: [{ id: "movie-1", title: "电影", itemType: "MOVIE" }],
      page: 1,
      pageSize: 24,
      total: 1,
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(createElement(
        QueryClientProvider,
        { client: queryClient },
        createElement(
          MemoryRouter,
          { initialEntries: ["/libraries/library-1"] },
          createElement(
            Routes,
            null,
            createElement(Route, { path: "/libraries/:libraryId", element: createElement(LibraryPage) }),
          ),
        ),
      ));
    });
    await act(async () => {
      await vi.waitFor(() => expect(container?.querySelector("[role='combobox'][aria-label='排序方式']")).toBeTruthy());
    });

    const sortBy = container.querySelector<HTMLButtonElement>("[role='combobox'][aria-label='排序方式']");
    expect(sortBy?.textContent).toContain("标题");
    await act(async () => {
      if (!sortBy) throw new Error("sort selector was not rendered");
      sortBy.click();
    });
    for (let index = 0; index < 2; index += 1) {
      await act(async () => {
        if (!sortBy) throw new Error("sort selector was not rendered");
        sortBy.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
      });
    }
    await act(async () => {
      if (!sortBy) throw new Error("sort selector was not rendered");
      sortBy.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    });
    await act(async () => {
      await vi.waitFor(() => expect(libraryItems).toHaveBeenCalledWith("library-1", 1, "MOVIE", {
        sortBy: "PremiereDate",
        sortOrder: "Descending",
      }));
    });
  });

  it("restores the selected sorting after the library page is reloaded", async () => {
    vi.spyOn(api, "libraries").mockResolvedValue({
      libraries: [{ id: "library-1", name: "电影", kind: "MOVIE" }],
    });
    const libraryItems = vi.spyOn(api, "libraryItems").mockResolvedValue({
      items: [{ id: "movie-1", title: "电影", itemType: "MOVIE" }],
      page: 1,
      pageSize: 24,
      total: 1,
    });

    container = document.createElement("div");
    document.body.append(container);
    const renderLibraryPage = async () => {
      root = createRoot(container as HTMLDivElement);
      const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
      await act(async () => {
        root?.render(createElement(
          QueryClientProvider,
          { client: queryClient },
          createElement(
            MemoryRouter,
            { initialEntries: ["/libraries/library-1"] },
            createElement(
              Routes,
              null,
              createElement(Route, { path: "/libraries/:libraryId", element: createElement(LibraryPage) }),
            ),
          ),
        ));
      });
    };

    await renderLibraryPage();
    await act(async () => {
      await vi.waitFor(() => expect(container?.querySelector("[role='combobox'][aria-label='排序方式']")).toBeTruthy());
    });

    const sortBy = container.querySelector<HTMLButtonElement>("[role='combobox'][aria-label='排序方式']");
    await act(async () => {
      if (!sortBy) throw new Error("sort selector was not rendered");
      sortBy.click();
    });
    await act(async () => {
      const releaseDateOption = document.querySelector<HTMLButtonElement>("[role='option'][data-value='PremiereDate']");
      if (!releaseDateOption) throw new Error("release-date sort option was not rendered");
      releaseDateOption.click();
      await vi.waitFor(() => expect(libraryItems).toHaveBeenCalledWith("library-1", 1, "MOVIE", {
        sortBy: "PremiereDate",
        sortOrder: "Descending",
      }));
    });

    await act(async () => root?.unmount());
    root = undefined;
    container.replaceChildren();
    libraryItems.mockClear();

    await renderLibraryPage();
    await act(async () => {
      await vi.waitFor(() => expect(libraryItems).toHaveBeenCalledWith("library-1", 1, "MOVIE", {
        sortBy: "PremiereDate",
        sortOrder: "Descending",
      }));
    });
    expect(container.querySelector("[role='combobox'][aria-label='排序方式']")?.textContent).toContain("发行日期");
  });

  it("opens with the pending metadata filter from a task result", async () => {
    vi.spyOn(api, "libraries").mockResolvedValue({
      libraries: [{ id: "library-1", name: "电影", kind: "MOVIE" }],
    });
    const libraryItems = vi.spyOn(api, "libraryItems").mockResolvedValue({
      items: [{ id: "movie-1", title: "待确认电影", itemType: "MOVIE", metadataPending: true }],
      page: 1,
      pageSize: 24,
      total: 1,
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(createElement(
        QueryClientProvider,
        { client: queryClient },
        createElement(
          MemoryRouter,
          { initialEntries: ["/libraries/library-1?metadataStatus=pending"] },
          createElement(
            Routes,
            null,
            createElement(Route, { path: "/libraries/:libraryId", element: createElement(LibraryPage) }),
          ),
        ),
      ));
    });

    await act(async () => {
      await vi.waitFor(() => expect(libraryItems).toHaveBeenCalledWith("library-1", 1, "MOVIE", {
        sortBy: "Name",
        sortOrder: "Ascending",
        metadataStatus: "PENDING",
      }));
    });
    expect(container.textContent).toContain("待确认");
    expect(container.querySelector(".lux-metadata-attention-badge")?.textContent).toContain("待确认");
  });

  it("shows batch confirmation only when every selected item is pending", async () => {
    vi.spyOn(api, "libraries").mockResolvedValue({
      libraries: [{ id: "library-1", name: "电影", kind: "MOVIE" }],
    });
    vi.spyOn(api, "libraryItems").mockResolvedValue({
      items: [
        { id: "pending-1", title: "待确认电影", itemType: "MOVIE", metadataPending: true },
        { id: "pending-2", title: "另一部待确认电影", itemType: "MOVIE", metadataPending: true },
      ],
      page: 1,
      pageSize: 24,
      total: 2,
    });
    const confirm = vi.spyOn(api, "confirmAdminMetadata").mockResolvedValue({ confirmedCount: 2, failedCount: 0, failedItemIds: [] });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(createElement(
        QueryClientProvider,
        { client: queryClient },
        createElement(
          MemoryRouter,
          { initialEntries: ["/libraries/library-1"] },
          createElement(
            Routes,
            null,
            createElement(Route, { path: "/libraries/:libraryId", element: createElement(LibraryPage) }),
          ),
        ),
      ));
    });
    await act(async () => {
      await vi.waitFor(() => expect(container?.querySelectorAll(".lux-media-card")).toHaveLength(2));
    });

    await act(async () => {
      container?.querySelector<HTMLButtonElement>("button[aria-label='开启媒体库多选']")?.click();
    });
    const checkboxes = container?.querySelectorAll<HTMLInputElement>(".lux-media-selection-checkbox");
    await act(async () => checkboxes?.forEach((checkbox) => checkbox.click()));
    expect(container?.textContent).toContain("批量确认");

    await act(async () => {
      container?.querySelector<HTMLButtonElement>("button[data-action='batch-confirm-metadata']")?.click();
    });
    expect(confirm).toHaveBeenCalledWith(["pending-1", "pending-2"]);
  });

  it("keeps the regular media menu when selected items are mixed", async () => {
    vi.spyOn(api, "libraries").mockResolvedValue({
      libraries: [{ id: "library-1", name: "电影", kind: "MOVIE" }],
    });
    vi.spyOn(api, "libraryItems").mockResolvedValue({
      items: [
        { id: "pending-1", title: "待确认电影", itemType: "MOVIE", metadataPending: true },
        { id: "confirmed-1", title: "已确认电影", itemType: "MOVIE", metadataPending: false },
      ],
      page: 1,
      pageSize: 24,
      total: 2,
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    await act(async () => {
      root?.render(createElement(
        QueryClientProvider,
        { client: queryClient },
        createElement(
          MemoryRouter,
          { initialEntries: ["/libraries/library-1"] },
          createElement(
            Routes,
            null,
            createElement(Route, { path: "/libraries/:libraryId", element: createElement(LibraryPage) }),
          ),
        ),
      ));
    });
    await act(async () => {
      await vi.waitFor(() => expect(container?.querySelectorAll(".lux-media-card")).toHaveLength(2));
    });
    await act(async () => {
      container?.querySelector<HTMLButtonElement>("button[aria-label='开启媒体库多选']")?.click();
      container?.querySelectorAll<HTMLInputElement>(".lux-media-selection-checkbox").forEach((checkbox) => checkbox.click());
    });

    expect(container?.querySelector("button[data-action='batch-confirm-metadata']")).toBeNull();
    expect(container?.querySelector("button[aria-label='打开 待确认电影 操作菜单']")).toBeTruthy();
    expect(container?.querySelector("button[aria-label='打开 已确认电影 操作菜单']")).toBeTruthy();
  });

  it("reports failed items when batch confirmation cannot confirm them", async () => {
    vi.spyOn(api, "libraries").mockResolvedValue({
      libraries: [{ id: "library-1", name: "电影", kind: "MOVIE" }],
    });
    vi.spyOn(api, "libraryItems").mockResolvedValue({
      items: [
        { id: "pending-1", title: "待确认电影", itemType: "MOVIE", metadataPending: true },
      ],
      page: 1,
      pageSize: 24,
      total: 1,
    });
    const confirm = vi.spyOn(api, "confirmAdminMetadata").mockResolvedValue({
      confirmedCount: 0,
      failedCount: 1,
      failedItemIds: ["pending-1"],
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    await act(async () => {
      root?.render(createElement(
        QueryClientProvider,
        { client: queryClient },
        createElement(
          MemoryRouter,
          { initialEntries: ["/libraries/library-1"] },
          createElement(
            Routes,
            null,
            createElement(Route, { path: "/libraries/:libraryId", element: createElement(LibraryPage) }),
          ),
        ),
      ));
    });
    await act(async () => {
      await vi.waitFor(() => expect(container?.querySelectorAll(".lux-media-card")).toHaveLength(1));
    });
    await act(async () => {
      container?.querySelector<HTMLButtonElement>("button[aria-label='开启媒体库多选']")?.click();
    });
    await act(async () => {
      await vi.waitFor(() => expect(container?.querySelector<HTMLInputElement>(".lux-media-selection-checkbox")).not.toBeNull());
      container?.querySelector<HTMLInputElement>(".lux-media-selection-checkbox")?.click();
    });
    await act(async () => {
      await vi.waitFor(() => expect(container?.querySelector("button[data-action='batch-confirm-metadata']")).not.toBeNull());
      container?.querySelector<HTMLButtonElement>("button[data-action='batch-confirm-metadata']")?.click();
    });

    await vi.waitFor(() => expect(confirm).toHaveBeenCalledWith(["pending-1"]));
    expect(container.textContent).toContain("1 项确认失败");
  });

  it("lets the administrator choose a primary item when merging selected versions", async () => {
    vi.spyOn(api, "libraries").mockResolvedValue({
      libraries: [{ id: "library-1", name: "电影", kind: "MOVIE" }],
    });
    vi.spyOn(api, "libraryItems").mockResolvedValue({
      items: [
        { id: "movie-1", title: "电影主版本", itemType: "MOVIE" },
        { id: "movie-2", title: "电影 4K", itemType: "MOVIE" },
      ],
      page: 1,
      pageSize: 24,
      total: 2,
    });
    const merge = vi.spyOn(api, "mergeAdminItems").mockResolvedValue({
      primaryItemId: "movie-2",
      mergedItemIds: ["movie-1"],
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    await act(async () => {
      root?.render(createElement(
        QueryClientProvider,
        { client: queryClient },
        createElement(
          MemoryRouter,
          { initialEntries: ["/libraries/library-1"] },
          createElement(
            Routes,
            null,
            createElement(Route, { path: "/libraries/:libraryId", element: createElement(LibraryPage) }),
          ),
        ),
      ));
    });
    await act(async () => {
      await vi.waitFor(() => expect(container?.querySelectorAll(".lux-media-card")).toHaveLength(2));
    });
    await act(async () => {
      container?.querySelector<HTMLButtonElement>("button[aria-label='开启媒体库多选']")?.click();
    });
    await act(async () => {
      await vi.waitFor(() => expect(container?.querySelectorAll<HTMLInputElement>(".lux-media-selection-checkbox")).toHaveLength(2));
      container?.querySelectorAll<HTMLInputElement>(".lux-media-selection-checkbox").forEach((checkbox) => checkbox.click());
    });
    await act(async () => {
      container?.querySelector<HTMLButtonElement>("button[data-action='merge-items']")?.click();
    });
    expect(container?.textContent).toContain("选择主条目");
    const primaryRadio = container?.querySelector<HTMLInputElement>("input[aria-label='将 电影 4K 设为主条目']");
    expect(primaryRadio).not.toBeNull();
    await act(async () => primaryRadio?.click());
    await act(async () => {
      container?.querySelector<HTMLButtonElement>("button[data-action='confirm-merge-items']")?.click();
    });
    await vi.waitFor(() => expect(merge).toHaveBeenCalledWith({
      itemIds: ["movie-1", "movie-2"],
      primaryItemId: "movie-2",
    }));
    expect(container?.textContent).toContain("已合并 1 个其他版本");
  });

  it("opens the virtual collection library with safe posters and read-only cards", async () => {
    const collectionLibraryId = "00000000-0000-7000-8000-000000000538";
    vi.spyOn(api, "libraries").mockResolvedValue({
      libraries: [{ id: collectionLibraryId, name: "合集", kind: "MIXED" }],
    });
    const libraryItems = vi.spyOn(api, "libraryItems").mockResolvedValue({
      items: [{
        id: "collection-1",
        title: "飞驰人生",
        itemType: "BOX_SET",
        memberCount: 3,
        posterItemId: "visible-movie-1",
        imageTags: { poster: "visible-poster-tag" },
      }],
      page: 1,
      pageSize: 24,
      total: 1,
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(createElement(
        QueryClientProvider,
        { client: queryClient },
        createElement(
          MemoryRouter,
          { initialEntries: [`/libraries/${collectionLibraryId}`] },
          createElement(
            Routes,
            null,
            createElement(Route, { path: "/libraries/:libraryId", element: createElement(LibraryPage) }),
          ),
        ),
      ));
    });
    await act(async () => {
      await vi.waitFor(() => expect(container?.querySelectorAll(".lux-media-card")).toHaveLength(1));
    });

    expect(libraryItems).toHaveBeenCalledWith(collectionLibraryId, 1, "BOX_SET", {
      sortBy: "Name",
      sortOrder: "Ascending",
    });
    expect(container.querySelector<HTMLAnchorElement>(".lux-media-card-link")?.getAttribute("href"))
      .toBe("/collections/collection-1");
    expect(container.querySelector<HTMLImageElement>(".lux-media-art img")?.getAttribute("src"))
      .toBe("/api/v1/items/visible-movie-1/images/poster?tag=visible-poster-tag");
    expect(container.textContent).toContain("3 部");
    expect(container.querySelector("button[aria-label='开启媒体库多选']")).toBeNull();
    expect(container.querySelector(".lux-media-card-menu-trigger")).toBeNull();
  });
});
