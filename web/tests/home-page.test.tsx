// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { HomePage } from "../src/features/home/HomePage";
import { api } from "../src/lib/api/client";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("HomePage shelves", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;

  beforeEach(() => {
    vi.spyOn(api, "itemImages").mockResolvedValue({ images: [] });
  });

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
    vi.restoreAllMocks();
  });

  it("shows accessible media libraries without a generic recently-added shelf", async () => {
    vi.spyOn(api, "home").mockResolvedValue({
      libraries: [{
        id: "library-1",
        name: "华语电影",
        kind: "MOVIE",
        coverImageUrl: "/covers/chinese.jpg",
        latest: [{ id: "latest-1", title: "最新华语片", itemType: "MOVIE" }],
      }],
      recommended: [],
      continueWatching: [{
        id: "resume-1",
        title: "继续中的电影",
        itemType: "MOVIE",
        runtimeTicks: 3_600_000_000,
        userData: { positionTicks: 1_800_000_000 },
      }],
      recentlyAdded: [{ id: "recent-1", title: "最近电影", itemType: "MOVIE" }],
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <HomePage />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(container.querySelector('[aria-label="我的媒体库"] .lux-library-card')?.textContent)
      .toContain("华语电影");
    expect(container.querySelector('[aria-label="我的媒体库"] .lux-horizontal-scroll-viewport')).not.toBeNull();
    expect(container.querySelector('[aria-label="最新华语电影"]')?.textContent)
      .toContain("最新华语片");
    expect(container.querySelector('[aria-label="最新华语电影"] .lux-horizontal-scroll-viewport')).not.toBeNull();
    expect([...container.querySelectorAll(".lux-home-content .lux-section h2")].map((heading) => heading.textContent))
      .toEqual(["我的媒体库", "继续观看", "最新华语电影"]);
    expect(container.querySelector('.lux-continue-card')?.textContent).toContain("继续中的电影");
    expect(container.querySelector('[aria-label="最近添加"]')).toBeNull();
  });

  it("keeps carousel controls in the same row as the playback actions", async () => {
    vi.spyOn(api, "home").mockResolvedValue({
      libraries: [],
      recommended: [
        { id: "featured-1", title: "精选电影", itemType: "MOVIE" },
        { id: "featured-2", title: "精选剧集", itemType: "SERIES" },
      ],
      continueWatching: [],
      recentlyAdded: [],
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <HomePage />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const actionRow = container.querySelector(".lux-hero-action-row");
    expect(actionRow).not.toBeNull();
    expect(actionRow?.querySelector(".lux-hero-actions")).not.toBeNull();
    expect(actionRow?.querySelector(".lux-hero-carousel-controls")).not.toBeNull();
    expect(actionRow?.querySelector(".lux-hero-carousel-controls")?.parentElement).toBe(actionRow);
  });

  it("uses an available media logo in the carousel title area", async () => {
    vi.spyOn(api, "home").mockResolvedValue({
      libraries: [],
      recommended: [{ id: "featured-1", title: "精选电影", itemType: "MOVIE" }],
      continueWatching: [],
      recentlyAdded: [],
    });
    vi.mocked(api.itemImages).mockResolvedValue({
      images: [{
        id: "logo-1",
        itemId: "featured-1",
        imageType: "LOGO",
        imageIndex: 0,
        url: "/api/v1/items/featured-1/images/logo",
      }],
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <HomePage />
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

    expect(container.querySelector<HTMLImageElement>(".lux-hero-logo")?.getAttribute("src"))
      .toBe("/api/v1/items/featured-1/images/logo");
    expect(container.querySelector(".lux-hero-title")?.textContent).toBe("");
    expect(container.querySelector(".lux-hero-title")?.querySelector("img")?.getAttribute("alt"))
      .toBe("精选电影");
  });
});
