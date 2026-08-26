// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { HomePage } from "../src/features/home/HomePage";
import { api } from "../src/lib/api/client";
import { queryKeys, queryRefreshIntervals } from "../src/lib/api/query-keys";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("HomePage shelves", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;
  const user = { id: "user-1", usernameNormalized: "viewer" };

  beforeEach(() => {
    localStorage.clear();
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
            <HomePage user={user} />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(container.querySelector('[aria-label="我的媒体库"] .lux-library-card')?.textContent)
      .toContain("华语电影");
    expect(container.querySelector<HTMLImageElement>(".lux-library-cover")?.getAttribute("decoding")).toBe("async");
    expect(container.querySelector('[aria-label="我的媒体库"] .lux-horizontal-scroll-viewport')).not.toBeNull();
    expect(container.querySelector('[aria-label="最新华语电影"]')?.textContent)
      .toContain("最新华语片");
    expect(container.querySelector('[aria-label="最新华语电影"] .lux-horizontal-scroll-viewport')).not.toBeNull();
    expect(container.querySelector<HTMLAnchorElement>('[aria-label="最新华语电影"] .lux-section-heading h2 a'))
      .toMatchObject({ href: `${window.location.origin}/libraries/library-1` });
    expect([...container.querySelectorAll(".lux-home-content .lux-section h2")].map((heading) => heading.textContent))
      .toEqual(["我的媒体库", "继续观看", "最新华语电影"]);
    expect(queryClient.getQueryCache().find({ queryKey: queryKeys.home })?.options.refetchInterval)
      .toBe(queryRefreshIntervals.mediaSurface);
    expect(queryClient.getQueryCache().find({ queryKey: queryKeys.home })?.options.refetchIntervalInBackground)
      .toBe(false);
    expect(container.querySelector('.lux-continue-card')?.textContent).toContain("继续中的电影");
    expect(container.querySelector('[aria-label="最近添加"]')).toBeNull();
  });

  it("renders homepage library shelves in the current account's saved order", async () => {
    vi.spyOn(api, "home").mockResolvedValue({
      libraries: [
        { id: "library-2", name: "剧集库", kind: "SERIES", latest: [{ id: "series-latest", title: "剧集最新", itemType: "SERIES" }] },
        { id: "library-1", name: "电影库", kind: "MOVIE", latest: [{ id: "movie-latest", title: "电影最新", itemType: "MOVIE" }] },
      ],
      recommended: [],
      continueWatching: [],
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <HomePage user={user} />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect([...container.querySelectorAll('[aria-label="我的媒体库"] .lux-library-card strong')].map((name) => name.textContent))
      .toEqual(["剧集库", "电影库"]);
    expect([...container.querySelectorAll(".lux-home-content .lux-section h2")].map((heading) => heading.textContent))
      .toEqual(["我的媒体库", "最新剧集库", "最新电影库"]);
  });

  it("renders every returned continue-watching item and shows the server total", async () => {
    vi.spyOn(api, "home").mockResolvedValue({
      libraries: [],
      recommended: [],
      continueWatching: [
        { id: "resume-1", title: "第一条", itemType: "MOVIE" },
        { id: "resume-2", title: "第二条", itemType: "MOVIE" },
        { id: "resume-3", title: "第三条", itemType: "MOVIE" },
      ],
      continueWatchingTotal: 3,
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
            <HomePage user={user} />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(container.querySelectorAll(".lux-continue-card")).toHaveLength(3);
    expect(container.querySelector("#continue-watching-heading")?.parentElement?.textContent)
      .toContain("3 项");
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
            <HomePage user={user} />
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

  it("starts an unfinished episode when the carousel highlights its series", async () => {
    vi.spyOn(api, "home").mockResolvedValue({
      libraries: [],
      recommended: [{ id: "series-1", title: "精选剧集", itemType: "SERIES" }],
      continueWatching: [{
        id: "episode-7",
        title: "未看完的一集",
        itemType: "EPISODE",
        seriesId: "series-1",
        userData: { positionTicks: 120_000_000 },
      }],
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
            <HomePage user={user} />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(container.querySelector<HTMLAnchorElement>(".lux-hero-actions a.lux-button-primary")?.getAttribute("href"))
      .toBe("/watch/episode-7");
    expect(container.querySelector(".lux-hero-actions a.lux-button-primary")?.textContent).toContain("继续播放");
  });

  it("starts the highlighted movie from the carousel instead of opening its detail page", async () => {
    vi.spyOn(api, "home").mockResolvedValue({
      libraries: [],
      recommended: [{ id: "movie-1", title: "精选电影", itemType: "MOVIE" }],
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
            <HomePage user={user} />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(container.querySelector<HTMLAnchorElement>(".lux-hero-actions a.lux-button-primary")?.getAttribute("href"))
      .toBe("/watch/movie-1");
  });

  it("applies the smallest title size while keeping the complete title text available", async () => {
    const title = "FC2-4916281 脸）强忍着因为嘘息而即将失禁，但在猛烈的冲击下不停地溢出";
    vi.spyOn(api, "home").mockResolvedValue({
      libraries: [],
      recommended: [{ id: "featured-long-title", title, itemType: "MOVIE" }],
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
            <HomePage user={user} />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const heroTitle = container.querySelector(".lux-hero-title");
    expect(heroTitle?.classList.contains("lux-hero-title--small")).toBe(true);
    expect(heroTitle?.querySelector(".lux-hero-title-text")?.textContent).toBe(title);
  });

  it("uses an available media logo in the carousel title area", async () => {
    vi.spyOn(api, "home").mockResolvedValue({
      libraries: [],
      recommended: [{ id: "featured-1", title: "精选电影", itemType: "MOVIE", imageTags: { logo: "logo-tag" } }],
      continueWatching: [],
      recentlyAdded: [],
    });
    vi.mocked(api.itemImages).mockRejectedValue(new Error("Hero should not request item images"));

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <HomePage user={user} />
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
      .toBe("/api/v1/items/featured-1/images/logo?tag=logo-tag");
    expect(container.querySelector<HTMLImageElement>(".lux-hero-logo")?.getAttribute("decoding")).toBe("async");
    expect(container.querySelector(".lux-hero-title")?.textContent).toBe("");
    expect(container.querySelector(".lux-hero-title")?.querySelector("img")?.getAttribute("alt"))
      .toBe("精选电影");
  });
});
