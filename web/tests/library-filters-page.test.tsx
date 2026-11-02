// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, createElement, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { LibraryPage } from "../src/features/library/LibraryPage";
import { api } from "../src/lib/api/client";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

function LocationProbe() {
  const location = useLocation();
  return createElement("output", { "data-testid": "location-search" }, location.search);
}

function libraryPageTree(initialEntry: string): ReactNode {
  return createElement(
    QueryClientProvider,
    { client: new QueryClient({ defaultOptions: { queries: { retry: false } } }) },
    createElement(
      MemoryRouter,
      { initialEntries: [initialEntry] },
      createElement(
        "div",
        null,
        createElement(LocationProbe),
        createElement(
          Routes,
          null,
          createElement(Route, { path: "/libraries/:libraryId", element: createElement(LibraryPage) }),
        ),
      ),
    ),
  );
}

describe("LibraryPage filters", () => {
  beforeEach(() => {
    vi.spyOn(window, "scrollTo").mockImplementation(() => {});
  });

  let container: HTMLDivElement | undefined;
  let root: Root | undefined;
  let triggerIntersection: (() => void) | undefined;

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
    document.body.replaceChildren();
    localStorage.clear();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    triggerIntersection = undefined;
  });

  async function renderLibraryPage(initialEntry: string) {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    await act(async () => {
      root?.render(libraryPageTree(initialEntry));
    });
    return container;
  }

  function locationSearch() {
    return container?.querySelector("[data-testid='location-search']")?.textContent ?? "";
  }

  async function waitForCards(count: number) {
    await act(async () => {
      await vi.waitFor(() => expect(container?.querySelectorAll(".lux-media-card")).toHaveLength(count));
    });
  }

  async function chooseSelectOption(triggerLabel: string, optionValue: string) {
    const trigger = container?.querySelector<HTMLButtonElement>(`[role='combobox'][aria-label='${triggerLabel}']`);
    expect(trigger, `${triggerLabel} selector`).toBeTruthy();
    await act(async () => trigger?.click());
    const option = document.querySelector<HTMLButtonElement>(`[role='option'][data-value='${optionValue}']`);
    expect(option, `${triggerLabel}=${optionValue} option`).toBeTruthy();
    await act(async () => option?.click());
  }

  function stubIntersectionObserver() {
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
  }

  it("restores every filter from the URL and forwards it to the API", async () => {
    vi.spyOn(api, "libraries").mockResolvedValue({
      libraries: [{ id: "library-1", name: "影视合集", kind: "MIXED" }],
    });
    const libraryItems = vi.spyOn(api, "libraryItems").mockResolvedValue({
      items: [{ id: "movie-1", title: "筛选后的电影", itemType: "MOVIE" }],
      page: 1,
      pageSize: 24,
      total: 1,
    });

    await renderLibraryPage("/libraries/library-1?type=MOVIE&year=2024&status=unplayed&favorite=1");
    await waitForCards(1);

    expect(libraryItems).toHaveBeenCalledWith("library-1", 1, "MOVIE", {
      sortBy: "Name",
      sortOrder: "Ascending",
      fields: "card",
      count: false,
      year: 2024,
      isPlayed: false,
      isFavorite: true,
    });
    expect(container?.querySelector<HTMLButtonElement>("[role='combobox'][aria-label='媒体类型']")?.textContent).toContain("电影");
  });

  it("only shows a type filter for libraries that mix movies and series", async () => {
    vi.spyOn(api, "libraries").mockResolvedValue({
      libraries: [{ id: "library-1", name: "电影", kind: "MOVIE" }],
    });
    vi.spyOn(api, "libraryItems").mockResolvedValue({
      items: [{ id: "movie-1", title: "示例电影", itemType: "MOVIE" }],
      page: 1,
      pageSize: 24,
      total: 1,
    });

    await renderLibraryPage("/libraries/library-1");
    await waitForCards(1);

    expect(container?.querySelector("[role='combobox'][aria-label='媒体类型']")).toBeNull();
    expect(container?.querySelector("input[aria-label='年份']")).not.toBeNull();
    expect(container?.querySelector("[role='combobox'][aria-label='观看状态']")).not.toBeNull();
    expect(container?.querySelector("[role='combobox'][aria-label='收藏筛选']")).not.toBeNull();
  });

  it("writes the selected type and watch state into the URL and refetches from the first page", async () => {
    stubIntersectionObserver();
    vi.spyOn(api, "libraries").mockResolvedValue({
      libraries: [{ id: "library-1", name: "影视合集", kind: "MIXED" }],
    });
    const libraryItems = vi.spyOn(api, "libraryItems").mockImplementation(async (_libraryId, page) => ({
      items: [{ id: `item-${page}`, title: `第 ${page} 页`, itemType: "MOVIE" }],
      page,
      pageSize: 24,
      total: 48,
    }));

    await renderLibraryPage("/libraries/library-1");
    await waitForCards(1);

    await act(async () => {
      triggerIntersection?.();
      await vi.waitFor(() => expect(container?.querySelectorAll(".lux-media-card")).toHaveLength(2));
    });
    expect(libraryItems).toHaveBeenCalledWith("library-1", 2, "MOVIE,SERIES", {
      sortBy: "Name",
      sortOrder: "Ascending",
      fields: "card",
      count: false,
    });

    await chooseSelectOption("媒体类型", "SERIES");
    await waitForCards(1);

    expect(locationSearch()).toContain("type=SERIES");
    expect(libraryItems).toHaveBeenLastCalledWith("library-1", 1, "SERIES", {
      sortBy: "Name",
      sortOrder: "Ascending",
      fields: "card",
      count: false,
    });

    await chooseSelectOption("观看状态", "UNPLAYED");
    await waitForCards(1);

    expect(locationSearch()).toContain("status=unplayed");
    expect(libraryItems).toHaveBeenLastCalledWith("library-1", 1, "SERIES", {
      sortBy: "Name",
      sortOrder: "Ascending",
      fields: "card",
      count: false,
      isPlayed: false,
    });
    expect(container?.textContent).not.toContain("第 2 页");
  });

  it("applies a typed year after confirming the input", async () => {
    vi.spyOn(api, "libraries").mockResolvedValue({
      libraries: [{ id: "library-1", name: "电影", kind: "MOVIE" }],
    });
    const libraryItems = vi.spyOn(api, "libraryItems").mockResolvedValue({
      items: [{ id: "movie-1", title: "示例电影", itemType: "MOVIE" }],
      page: 1,
      pageSize: 24,
      total: 1,
    });

    await renderLibraryPage("/libraries/library-1");
    await waitForCards(1);

    const yearInput = container?.querySelector<HTMLInputElement>("input[aria-label='年份']");
    expect(yearInput).toBeTruthy();
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(yearInput, "2024");
      yearInput?.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await act(async () => {
      yearInput?.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    });

    await act(async () => {
      await vi.waitFor(() => expect(libraryItems).toHaveBeenLastCalledWith("library-1", 1, "MOVIE,SERIES", {
        sortBy: "Name",
        sortOrder: "Ascending",
        fields: "card",
        count: false,
        year: 2024,
      }));
    });
    expect(locationSearch()).toContain("year=2024");
  });

  it("clears every filter back to the unfiltered list", async () => {
    vi.spyOn(api, "libraries").mockResolvedValue({
      libraries: [{ id: "library-1", name: "影视合集", kind: "MIXED" }],
    });
    const libraryItems = vi.spyOn(api, "libraryItems").mockResolvedValue({
      items: [{ id: "movie-1", title: "示例电影", itemType: "MOVIE" }],
      page: 1,
      pageSize: 24,
      total: 1,
    });

    await renderLibraryPage("/libraries/library-1?type=SERIES&year=2024&status=played&favorite=1");
    await waitForCards(1);

    const clearButton = container?.querySelector<HTMLButtonElement>("button[data-action='clear-library-filters']");
    expect(clearButton).toBeTruthy();
    await act(async () => clearButton?.click());

    await act(async () => {
      await vi.waitFor(() => expect(libraryItems).toHaveBeenLastCalledWith("library-1", 1, "MOVIE,SERIES", {
        sortBy: "Name",
        sortOrder: "Ascending",
        fields: "card",
        count: false,
      }));
    });
    expect(locationSearch()).toBe("");
    expect(container?.querySelector("button[data-action='clear-library-filters']")).toBeNull();
  });

  it("keeps unknown filter values out of the request", async () => {
    vi.spyOn(api, "libraries").mockResolvedValue({
      libraries: [{ id: "library-1", name: "影视合集", kind: "MIXED" }],
    });
    const libraryItems = vi.spyOn(api, "libraryItems").mockResolvedValue({
      items: [{ id: "movie-1", title: "示例电影", itemType: "MOVIE" }],
      page: 1,
      pageSize: 24,
      total: 1,
    });

    await renderLibraryPage("/libraries/library-1?type=VIDEO&year=abcd&status=maybe&favorite=0");
    await waitForCards(1);

    expect(libraryItems).toHaveBeenLastCalledWith("library-1", 1, "MOVIE,SERIES", {
      sortBy: "Name",
      sortOrder: "Ascending",
      fields: "card",
      count: false,
    });
    expect(container?.querySelector("button[data-action='clear-library-filters']")).toBeNull();
  });

  it("keeps the global collection library unfiltered even when the URL carries filter parameters", async () => {
    const collectionLibraryId = "00000000-0000-7000-8000-000000000538";
    vi.spyOn(api, "libraries").mockResolvedValue({
      libraries: [{ id: collectionLibraryId, name: "合集", kind: "MIXED" }],
    });
    const libraryItems = vi.spyOn(api, "libraryItems").mockResolvedValue({
      items: [{ id: "collection-1", title: "飞驰人生", itemType: "BOX_SET" }],
      page: 1,
      pageSize: 24,
      total: 1,
    });

    await renderLibraryPage(`/libraries/${collectionLibraryId}?type=MOVIE&year=2024&status=played&favorite=1`);
    await waitForCards(1);

    expect(libraryItems).toHaveBeenLastCalledWith(collectionLibraryId, 1, "BOX_SET", {
      sortBy: "Name",
      sortOrder: "Ascending",
      fields: "card",
      count: false,
    });
    expect(container?.querySelector(".lux-library-filter-toolbar")).toBeNull();
  });

  it("explains an empty result that comes from an active filter", async () => {
    vi.spyOn(api, "libraries").mockResolvedValue({
      libraries: [{ id: "library-1", name: "电影", kind: "MOVIE" }],
    });
    vi.spyOn(api, "libraryItems").mockResolvedValue({
      items: [],
      page: 1,
      pageSize: 24,
      total: 0,
    });

    await renderLibraryPage("/libraries/library-1?year=2024");
    await act(async () => {
      await vi.waitFor(() => expect(container?.textContent).toContain("没有符合筛选条件的内容"));
    });
  });
});
