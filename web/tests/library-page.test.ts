// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { LibraryPage, libraryItemTypeFilter } from "../src/features/library/LibraryPage";
import { api } from "../src/lib/api/client";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

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
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;
  let triggerIntersection: (() => void) | undefined;

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
    localStorage.clear();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    triggerIntersection = undefined;
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
    expect(badge?.classList.contains("is-compact")).toBe(true);
    expect(badge?.textContent).toBe("6.7");
    expect(badge?.querySelector(".lux-rating-source")).toBeNull();
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
});
