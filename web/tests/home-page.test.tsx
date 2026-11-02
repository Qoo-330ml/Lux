// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { HomePage, homeRefetchInterval } from "../src/features/home/HomePage";
import { HERO_CAROUSEL_INTERVAL_MS } from "../src/features/home/carousel";
import { api } from "../src/lib/api/client";
import type { HomeResponse } from "../src/lib/api/types";
import { queryKeys, queryRefreshIntervals } from "../src/lib/api/query-keys";
import { accountSettingsStorageKey } from "../src/features/account/account-settings";

const heroSlidesTracker = vi.hoisted(() => vi.fn());
const heroTitleScaleTracker = vi.hoisted(() => vi.fn());

vi.mock("../src/features/home/carousel", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../src/features/home/carousel")>();
  return {
    ...actual,
    heroSlides: (...args: Parameters<typeof actual.heroSlides>) => {
      heroSlidesTracker(...args);
      return actual.heroSlides(...args);
    },
    heroTitleScale: (title: string) => {
      heroTitleScaleTracker(title);
      return actual.heroTitleScale(title);
    },
  };
});

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

function mockHomeSections(response: HomeResponse) {
  vi.spyOn(api, "homeCarousel").mockResolvedValue({ recommended: response.recommended ?? [] });
  vi.spyOn(api, "homeLibraries").mockResolvedValue({ libraries: response.libraries ?? [] });
  const continueWatching = response.continueWatching ?? [];
  vi.spyOn(api, "homeContinueWatching").mockResolvedValue({
    items: continueWatching,
    total: response.continueWatchingTotal ?? continueWatching.length,
  });
  return vi.spyOn(api, "homeLibrariesLatest").mockImplementation(async (libraryIds) => ({
    libraries: libraryIds.map((libraryId) => ({
      libraryId,
      items: response.libraries?.find((library) => library.id === libraryId)?.latest ?? [],
    })),
  }));
}

function waitForHomeUpdate(assertion: () => unknown) {
  return vi.waitFor(assertion, { timeout: 3_000 });
}

function setDocumentVisibility(state: "visible" | "hidden") {
  Object.defineProperty(document, "visibilityState", { configurable: true, get: () => state });
}

describe("HomePage shelves", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;
  const user = { id: "user-1", usernameNormalized: "viewer" };

  beforeEach(() => {
    localStorage.clear();
    sessionStorage.clear();
    vi.spyOn(api, "itemImages").mockResolvedValue({ images: [] });
  });

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
    vi.restoreAllMocks();
  });

  it("renders independent library shelves when the carousel request times out", async () => {
    vi.spyOn(api, "homeCarousel").mockRejectedValue(new Error("轮播请求超时"));
    vi.spyOn(api, "homeLibraries").mockResolvedValue({
      libraries: [{ id: "library-1", name: "电影库", kind: "MOVIE" }],
    });
    vi.spyOn(api, "homeContinueWatching").mockResolvedValue({ items: [], total: 0 });
    vi.spyOn(api, "homeLibraryLatest");
    vi.spyOn(api, "homeLibrariesLatest").mockResolvedValue({
      libraries: [{
        libraryId: "library-1",
        items: [{
          id: "scraped-movie",
          title: "刚刮削完成",
          itemType: "MOVIE",
          imageTags: { poster: "poster-v2" },
        }],
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
            <HomePage user={user} />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(container.querySelector(".lux-skeleton-page")).toBeNull();
    expect(container.querySelector('[aria-label="我的媒体库"] .lux-library-card')?.textContent)
      .toContain("电影库");
    await waitForHomeUpdate(() => expect(container.querySelector('[aria-label="最新电影库"]')?.textContent)
      .toContain("刚刮削完成"));
    expect(container.querySelector<HTMLImageElement>(
      '[aria-label="最新电影库"] .lux-media-card img',
    )?.src).toContain("tag=poster-v2");

    expect(sessionStorage.getItem("lux.home-carousel.v2:user-1")).toBeNull();
  });

  it("shows accessible media libraries without a generic recently-added shelf", async () => {
    mockHomeSections({
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
    await waitForHomeUpdate(() => expect(container.querySelector('[aria-label="最新华语电影"]')?.textContent)
      .toContain("最新华语片"));
    await waitForHomeUpdate(() => expect(container.querySelector('.lux-continue-card')?.textContent).toContain("继续中的电影"));
    expect(container.querySelector('[aria-label="最新华语电影"] .lux-horizontal-scroll-viewport')).not.toBeNull();
    expect(container.querySelector<HTMLAnchorElement>('[aria-label="最新华语电影"] .lux-section-heading h2 a'))
      .toMatchObject({ href: `${window.location.origin}/libraries/library-1` });
    expect([...container.querySelectorAll(".lux-home-content .lux-section h2")].map((heading) => heading.textContent))
      .toEqual(["我的媒体库", "继续观看", "最新华语电影"]);
    const loadedHomeQuery = queryClient.getQueryCache().find({ queryKey: queryKeys.homeCarousel });
    expect(homeRefetchInterval(loadedHomeQuery?.state.data)).toBe(queryRefreshIntervals.mediaSurface);
    expect(loadedHomeQuery?.options.refetchIntervalInBackground).toBe(false);
    expect(container.querySelector('.lux-continue-card')?.textContent).toContain("继续中的电影");
    expect(container.querySelector('[aria-label="最近添加"]')).toBeNull();
  });

  it("keeps other homepage sections visible when the carousel request times out", async () => {
    const homeRequest = vi.spyOn(api, "homeCarousel")
      .mockRejectedValueOnce(new Error("首页请求超时，请重试"))
      .mockResolvedValueOnce({ recommended: [] });
    vi.spyOn(api, "homeLibraries").mockResolvedValue({ libraries: [] });
    vi.spyOn(api, "homeContinueWatching").mockResolvedValue({ items: [], total: 0 });
    vi.spyOn(api, "homeLibraryLatest").mockResolvedValue({ items: [] });
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

    expect(container.querySelector(".lux-skeleton-page")).toBeNull();
    expect(container.querySelector(".lux-page-state")).toBeNull();
    expect(container.querySelector('[role="status"]')?.textContent).toContain("精选轮播加载失败");
    const retry = container.querySelector<HTMLButtonElement>('[role="status"] button');
    expect(retry?.textContent).toContain("重试");
    const failedHomeQuery = queryClient.getQueryCache().find({ queryKey: queryKeys.homeCarousel });
    expect(homeRefetchInterval(failedHomeQuery?.state.data)).toBe(false);

    await act(async () => {
      retry?.click();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(homeRequest).toHaveBeenCalledTimes(2);
    expect(container.querySelector(".lux-page-state")).toBeNull();
    const loadedHomeQuery = queryClient.getQueryCache().find({ queryKey: queryKeys.homeCarousel });
    expect(homeRefetchInterval(loadedHomeQuery?.state.data)).toBe(queryRefreshIntervals.mediaSurface);
  });

  it("renders homepage library shelves in the current account's saved order", async () => {
    const latest = mockHomeSections({
      libraries: [
        { id: "library-2", name: "剧集库", kind: "SERIES", latest: [{ id: "series-latest", title: "剧集最新", itemType: "SERIES" }] },
        { id: "library-1", name: "电影库", kind: "MOVIE", latest: [{ id: "movie-latest", title: "电影最新", itemType: "MOVIE" }] },
      ],
      recommended: [],
      continueWatching: [],
    });
    const legacyLatest = vi.spyOn(api, "homeLibraryLatest");

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
    expect(latest).toHaveBeenCalledTimes(1);
    expect(latest.mock.calls[0]?.[0]).toEqual(["library-2", "library-1"]);
    await waitForHomeUpdate(() => expect(container.querySelector('[aria-label="最新剧集库"]')?.textContent)
      .toContain("剧集最新"));
    await waitForHomeUpdate(() => expect(container.querySelector('[aria-label="最新电影库"]')?.textContent)
      .toContain("电影最新"));
    expect(legacyLatest).not.toHaveBeenCalled();
    const latestQuery = queryClient.getQueryCache().find({
      queryKey: queryKeys.homeLatestLibraries(["library-2", "library-1"]),
      exact: true,
    });
    expect(homeRefetchInterval(latestQuery?.state.data)).toBe(queryRefreshIntervals.mediaSurface);
    expect(queryKeys.homeLatestLibraries(["library-2", "library-1"]))
      .not.toEqual(queryKeys.homeLatestLibraries(["library-1", "library-2"]));
  });

  it("hides homepage media-library and continue-watching sections when the account disables them", async () => {
    localStorage.setItem(accountSettingsStorageKey(user.id), JSON.stringify({
      showMediaLibraries: false,
      showContinueWatching: false,
    }));
    mockHomeSections({
      libraries: [{
        id: "library-1",
        name: "电影库",
        kind: "MOVIE",
        latest: [{ id: "latest-1", title: "最新电影", itemType: "MOVIE" }],
      }],
      recommended: [],
      continueWatching: [{ id: "resume-1", title: "继续中的电影", itemType: "MOVIE" }],
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

    expect(container.querySelector('[aria-label="我的媒体库"]')).toBeNull();
    expect(container.querySelector('[aria-label="继续观看"]')).toBeNull();
    expect(container.querySelector('[aria-label="最新电影库"]')).not.toBeNull();
  });

  it("renders every returned continue-watching item and shows the server total", async () => {
    const latest = mockHomeSections({
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
    expect(latest).not.toHaveBeenCalled();
  });

  it("keeps other homepage sections visible when the batched latest request fails", async () => {
    vi.spyOn(api, "homeCarousel").mockResolvedValue({ recommended: [] });
    vi.spyOn(api, "homeLibraries").mockResolvedValue({ libraries: [
      { id: "library-1", name: "电影库", kind: "MOVIE" },
      { id: "library-2", name: "剧集库", kind: "SERIES" },
    ] });
    vi.spyOn(api, "homeContinueWatching").mockResolvedValue({
      items: [{ id: "resume-1", title: "继续中的电影", itemType: "MOVIE" }],
      total: 1,
    });
    const latest = vi.spyOn(api, "homeLibrariesLatest").mockRejectedValue(new Error("最新资源接口暂不可用"));

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
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    await waitForHomeUpdate(() => expect(latest).toHaveBeenCalledTimes(1));
    expect(container.querySelector('[aria-label="我的媒体库"] .lux-library-card')?.textContent)
      .toContain("电影库");
    expect(container.querySelector(".lux-continue-card")?.textContent)
      .toContain("继续中的电影");
    expect(container.querySelector('[aria-label="最新电影库"] [role="alert"]')?.textContent)
      .toContain("最新资源加载失败");
    expect(container.querySelector('[aria-label="最新剧集库"] [role="alert"]')?.textContent)
      .toContain("最新资源加载失败");
  });

  it("keeps carousel controls in the same row as the playback actions", async () => {
    mockHomeSections({
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

    await waitForHomeUpdate(() => expect(container.querySelector(".lux-hero-carousel-controls")).not.toBeNull());
    const actionRow = container.querySelector(".lux-hero-action-row");
    expect(actionRow).not.toBeNull();
    expect(actionRow?.querySelector(".lux-hero-actions")).not.toBeNull();
    expect(actionRow?.querySelector(".lux-hero-carousel-controls")).not.toBeNull();
    expect(actionRow?.querySelector(".lux-hero-carousel-controls")?.parentElement).toBe(actionRow);
  });

  it("uses clickable dots without arrows for quick selection", async () => {
    mockHomeSections({
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

    await waitForHomeUpdate(() => expect(container.querySelector(".lux-hero-carousel-controls")).not.toBeNull());
    expect(container.querySelector(".lux-hero-copy")?.getAttribute("aria-label")).toContain("精选电影");
    expect(container.querySelector(".lux-hero-copy-animated")).not.toBeNull();
    const controls = container.querySelector(".lux-hero-carousel-controls");
    expect(controls?.querySelectorAll(".lux-hero-carousel-arrow")).toHaveLength(0);
    expect(controls?.querySelectorAll(".lux-hero-dot")).toHaveLength(2);
    expect(controls?.querySelector(".lux-hero-dot.is-active .lux-hero-dot-progress")).not.toBeNull();

    const dots = controls?.querySelectorAll<HTMLButtonElement>(".lux-hero-dot");
    await act(async () => {
      dots?.[1].click();
    });
    await waitForHomeUpdate(() => expect(container.querySelectorAll<HTMLButtonElement>(".lux-hero-dot")[1]?.getAttribute("aria-current"))
      .toBe("true"));
    await waitForHomeUpdate(() => expect(container.querySelector(".lux-hero-copy")?.getAttribute("aria-label"))
      .toContain("精选剧集"));
    const nextDots = container.querySelectorAll<HTMLButtonElement>(".lux-hero-dot");
    expect(nextDots[1].getAttribute("aria-current")).toBe("true");
    expect(nextDots[1].querySelector(".lux-hero-dot-progress")).not.toBeNull();
  });

  it("advances to the next slide after the active dot's fill duration", async () => {
    try {
      mockHomeSections({
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
      await waitForHomeUpdate(() => expect(container.querySelector<HTMLButtonElement>(".lux-hero-dot.is-active")?.getAttribute("aria-label"))
        .toContain("第 1 条精选"));

      vi.useFakeTimers();
      await act(async () => {
        container.querySelectorAll<HTMLButtonElement>(".lux-hero-dot")[1]?.click();
        await Promise.resolve();
      });
      await act(async () => {
        vi.advanceTimersByTime(HERO_CAROUSEL_INTERVAL_MS);
        vi.advanceTimersByTime(450);
      });
      expect(container.querySelector<HTMLButtonElement>(".lux-hero-dot.is-active")?.getAttribute("aria-label"))
        .toContain("第 1 条精选");
    } finally {
      if (vi.isFakeTimers()) {
        act(() => root?.unmount());
        root = undefined;
      }
      vi.useRealTimers();
    }
  });

  async function mountCarousel(recommended: NonNullable<HomeResponse["recommended"]> = [
    { id: "featured-1", title: "精选电影", itemType: "MOVIE" },
    { id: "featured-2", title: "精选剧集", itemType: "SERIES" },
  ]) {
    mockHomeSections({
      libraries: [],
      recommended,
      continueWatching: [],
      recentlyAdded: [],
    });

    const element = document.createElement("div");
    document.body.append(element);
    container = element;
    root = createRoot(element);
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
    await waitForHomeUpdate(() => expect(element.querySelector<HTMLButtonElement>(".lux-hero-dot.is-active")?.getAttribute("aria-label"))
      .toContain("第 1 条精选"));
    return element;
  }

  function activeSlideLabel(element: HTMLElement) {
    return element.querySelector<HTMLButtonElement>(".lux-hero-dot.is-active")?.getAttribute("aria-label");
  }

  function dispatchPointer(
    target: Element,
    type: "pointerover" | "pointerout",
    pointerType: string,
    relatedTarget: EventTarget | null = null,
  ) {
    const event = new MouseEvent(type, { bubbles: true, relatedTarget: relatedTarget as Node | null });
    Object.defineProperty(event, "pointerType", { configurable: true, value: pointerType });
    target.dispatchEvent(event);
  }

  /** jsdom implements neither pointer-type nor media-query based hover/focus capabilities. */
  function stubPointerCapabilities({
    hoverSelector = false,
    hoverMedia = false,
    focusVisible = false,
  }: { hoverSelector?: boolean; hoverMedia?: boolean; focusVisible?: boolean } = {}) {
    const originalMatches = Element.prototype.matches;
    const originalMatchMedia = window.matchMedia;
    vi.spyOn(Element.prototype, "matches").mockImplementation(function (this: Element, selector: string) {
      if (selector === ":hover") return hoverSelector;
      if (selector === ":focus-visible") return focusVisible;
      return originalMatches.call(this, selector);
    });
    Object.defineProperty(window, "matchMedia", {
      configurable: true,
      writable: true,
      value: (query: string) => ({
        matches: hoverMedia,
        media: query,
        onchange: null,
        addListener: () => undefined,
        removeListener: () => undefined,
        addEventListener: () => undefined,
        removeEventListener: () => undefined,
        dispatchEvent: () => false,
      }),
    });
    return () => {
      Object.defineProperty(window, "matchMedia", {
        configurable: true,
        writable: true,
        value: originalMatchMedia,
      });
    };
  }

  function progressIsPaused(element: HTMLElement) {
    return element.querySelector(".lux-hero-dot-progress")?.classList.contains("is-paused");
  }

  it("pauses the carousel while the mouse pointer rests on the hero and resumes afterwards", async () => {
    const restoreCapabilities = stubPointerCapabilities({ hoverMedia: true });
    try {
      const element = await mountCarousel();
      const hero = element.querySelector(".lux-hero") as HTMLElement;

      try {
        vi.useFakeTimers({ now: Date.now() });
        act(() => dispatchPointer(hero, "pointerover", "mouse", document.body));
        expect(progressIsPaused(element)).toBe(true);

        act(() => vi.advanceTimersByTime(HERO_CAROUSEL_INTERVAL_MS * 3));
        expect(activeSlideLabel(element)).toContain("第 1 条精选");

        act(() => dispatchPointer(hero, "pointerout", "mouse", document.body));
        expect(progressIsPaused(element)).toBe(false);

        act(() => vi.advanceTimersByTime(HERO_CAROUSEL_INTERVAL_MS));
        expect(activeSlideLabel(element)).toContain("第 2 条精选");
      } finally {
        if (vi.isFakeTimers()) {
          act(() => root?.unmount());
          root = undefined;
        }
        vi.useRealTimers();
      }
    } finally {
      restoreCapabilities();
    }
  });

  it("ignores touch pointer events so a tap cannot stick the carousel paused", async () => {
    const restoreCapabilities = stubPointerCapabilities({ hoverMedia: false });
    try {
      const element = await mountCarousel();
      const hero = element.querySelector(".lux-hero") as HTMLElement;

      try {
        vi.useFakeTimers({ now: Date.now() });
        act(() => element.querySelectorAll<HTMLButtonElement>(".lux-hero-dot")[1]?.click());
        await act(async () => {});
        expect(activeSlideLabel(element)).toContain("第 2 条精选");

        act(() => {
          // A real tap also synthesises mouse compatibility events that never fire a matching
          // mouseleave, which used to leave `hovered` stuck at true.
          hero.dispatchEvent(new MouseEvent("mouseover", { bubbles: true, relatedTarget: document.body }));
          dispatchPointer(hero, "pointerover", "touch", document.body);
        });
        expect(progressIsPaused(element)).toBe(false);

        act(() => vi.advanceTimersByTime(HERO_CAROUSEL_INTERVAL_MS));
        expect(activeSlideLabel(element)).toContain("第 1 条精选");

        act(() => dispatchPointer(hero, "pointerover", "mouse", document.body));
        expect(progressIsPaused(element)).toBe(true);
        act(() => vi.advanceTimersByTime(HERO_CAROUSEL_INTERVAL_MS * 2));
        expect(activeSlideLabel(element)).toContain("第 1 条精选");
      } finally {
        if (vi.isFakeTimers()) {
          act(() => root?.unmount());
          root = undefined;
        }
        vi.useRealTimers();
      }
    } finally {
      restoreCapabilities();
    }
  });

  it("pauses the carousel while keyboard focus is inside the hero", async () => {
    const restoreCapabilities = stubPointerCapabilities({ focusVisible: true });
    try {
      const element = await mountCarousel();
      const dot = element.querySelectorAll<HTMLButtonElement>(".lux-hero-dot")[0];

      try {
        vi.useFakeTimers({ now: Date.now() });
        act(() => dot.focus());
        expect(progressIsPaused(element)).toBe(true);

        act(() => vi.advanceTimersByTime(HERO_CAROUSEL_INTERVAL_MS * 3));
        expect(activeSlideLabel(element)).toContain("第 1 条精选");

        act(() => dot.blur());
        expect(progressIsPaused(element)).toBe(false);

        act(() => vi.advanceTimersByTime(HERO_CAROUSEL_INTERVAL_MS));
        expect(activeSlideLabel(element)).toContain("第 2 条精选");
      } finally {
        if (vi.isFakeTimers()) {
          act(() => root?.unmount());
          root = undefined;
        }
        vi.useRealTimers();
      }
    } finally {
      restoreCapabilities();
    }
  });

  it("keeps rotating after a mouse click focuses a dot that is not focus-visible", async () => {
    const restoreCapabilities = stubPointerCapabilities({ focusVisible: false, hoverMedia: true });
    try {
      const element = await mountCarousel();
      const hero = element.querySelector(".lux-hero") as HTMLElement;
      const dot = element.querySelectorAll<HTMLButtonElement>(".lux-hero-dot")[0];

      try {
        vi.useFakeTimers({ now: Date.now() });
        act(() => dispatchPointer(hero, "pointerover", "mouse", document.body));
        act(() => dot.focus());
        act(() => dispatchPointer(hero, "pointerout", "mouse", document.body));

        expect(dot.matches(":focus-visible")).toBe(false);
        expect(progressIsPaused(element)).toBe(false);

        act(() => vi.advanceTimersByTime(HERO_CAROUSEL_INTERVAL_MS));
        expect(activeSlideLabel(element)).toContain("第 2 条精选");
      } finally {
        if (vi.isFakeTimers()) {
          act(() => root?.unmount());
          root = undefined;
        }
        vi.useRealTimers();
      }
    } finally {
      restoreCapabilities();
    }
  });

  it("starts paused when a remounted hover-capable carousel is already under the pointer", async () => {
    const restoreCapabilities = stubPointerCapabilities({ hoverSelector: true, hoverMedia: true });
    try {
      const element = await mountCarousel();
      expect(progressIsPaused(element)).toBe(true);
    } finally {
      restoreCapabilities();
    }
  });

  it("does not adopt a sticky :hover state when the device cannot hover", async () => {
    const restoreCapabilities = stubPointerCapabilities({ hoverSelector: true, hoverMedia: false });
    try {
      const element = await mountCarousel();
      expect(progressIsPaused(element)).toBe(false);
    } finally {
      restoreCapabilities();
    }
  });

  it("pauses the carousel while the page is hidden and resumes when it is visible again", async () => {
    const element = await mountCarousel();

    try {
      vi.useFakeTimers({ now: Date.now() });
      act(() => {
        setDocumentVisibility("hidden");
        document.dispatchEvent(new Event("visibilitychange"));
      });
      act(() => vi.advanceTimersByTime(HERO_CAROUSEL_INTERVAL_MS * 3));
      expect(activeSlideLabel(element)).toContain("第 1 条精选");

      act(() => {
        setDocumentVisibility("visible");
        document.dispatchEvent(new Event("visibilitychange"));
      });
      act(() => vi.advanceTimersByTime(HERO_CAROUSEL_INTERVAL_MS));
      expect(activeSlideLabel(element)).toContain("第 2 条精选");
    } finally {
      setDocumentVisibility("visible");
      if (vi.isFakeTimers()) {
        act(() => root?.unmount());
        root = undefined;
      }
      vi.useRealTimers();
    }
  });

  it("preloads only the next slide backdrop and keeps the visible image eager", async () => {
    const created: { src: string }[] = [];

    class TrackingImage {
      src = "";
      decoding = "";
      onload: (() => void) | null = null;
      onerror: (() => void) | null = null;
      constructor() { created.push(this); }
      decode() { return Promise.resolve(); }
    }

    vi.stubGlobal("Image", TrackingImage);
    try {
      const element = await mountCarousel([
        { id: "featured-1", title: "精选电影", itemType: "MOVIE", imageTags: { fanart: "fa-1" } },
        { id: "featured-2", title: "精选剧集", itemType: "SERIES", imageTags: { fanart: "fa-2" } },
        { id: "featured-3", title: "精选纪录片", itemType: "MOVIE", imageTags: { fanart: "fa-3" } },
      ]);

      const backdrop = element.querySelector<HTMLImageElement>(".lux-hero-backdrop");
      expect(backdrop?.getAttribute("src")).toBe("/api/v1/items/featured-1/images/fanart?tag=fa-1");
      expect(backdrop?.getAttribute("fetchpriority")).toBe("high");
      expect(created.map((image) => image.src))
        .toEqual(["/api/v1/items/featured-2/images/fanart?tag=fa-2"]);

      act(() => element.querySelectorAll<HTMLButtonElement>(".lux-hero-dot")[2]?.click());
      await waitForHomeUpdate(() => expect(activeSlideLabel(element)).toContain("第 3 条精选"));

      expect(created.map((image) => image.src)).toEqual([
        "/api/v1/items/featured-2/images/fanart?tag=fa-2",
        "/api/v1/items/featured-1/images/fanart?tag=fa-1",
      ]);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("starts a changed slide list from its first slide instead of a stale index", async () => {
    const firstSlides = [
      { id: "featured-1", title: "精选电影", itemType: "MOVIE" },
      { id: "featured-2", title: "精选剧集", itemType: "SERIES" },
      { id: "featured-3", title: "精选纪录片", itemType: "MOVIE" },
    ];
    const nextSlides = [
      { id: "fresh-1", title: "新精选甲", itemType: "MOVIE" },
      { id: "fresh-2", title: "新精选乙", itemType: "SERIES" },
      { id: "fresh-3", title: "新精选丙", itemType: "MOVIE" },
      { id: "fresh-4", title: "新精选丁", itemType: "SERIES" },
    ];
    const homeCarousel = vi.spyOn(api, "homeCarousel").mockResolvedValue({ recommended: firstSlides });
    vi.spyOn(api, "homeLibraries").mockResolvedValue({ libraries: [] });
    vi.spyOn(api, "homeContinueWatching").mockResolvedValue({ items: [], total: 0 });
    vi.spyOn(api, "homeLibrariesLatest").mockResolvedValue({ libraries: [] });
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    const element = document.createElement("div");
    document.body.append(element);
    container = element;
    root = createRoot(element);
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
    await waitForHomeUpdate(() => expect(activeSlideLabel(element)).toContain("第 1 条精选"));

    act(() => element.querySelectorAll<HTMLButtonElement>(".lux-hero-dot")[2]?.click());
    await waitForHomeUpdate(() => expect(activeSlideLabel(element)).toContain("第 3 条精选"));

    heroTitleScaleTracker.mockClear();
    homeCarousel.mockResolvedValue({ recommended: nextSlides });
    await act(async () => {
      await queryClient.invalidateQueries({ queryKey: queryKeys.homeCarousel });
    });
    await waitForHomeUpdate(() => expect(element.querySelector(".lux-hero-title-text")?.textContent).toBe("新精选甲"));

    const renderedTitles = heroTitleScaleTracker.mock.calls.map(([title]) => title);
    expect(renderedTitles).toContain("新精选甲");
    expect(renderedTitles).not.toContain("新精选丙");
    expect(activeSlideLabel(element)).toContain("第 1 条精选");
    expect(element.querySelectorAll(".lux-hero-dot")).toHaveLength(4);
  });

  it("starts an unfinished episode when the carousel highlights its series", async () => {
    mockHomeSections({
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

    await waitForHomeUpdate(() => expect(container.querySelector<HTMLAnchorElement>(".lux-hero-actions a.lux-button-primary")?.getAttribute("href"))
      .toBe("/watch/episode-7"));
    expect(container.querySelector(".lux-hero-actions a.lux-button-primary")?.textContent).toContain("继续播放");
  });

  it("does not promote a continue-watching episode into the carousel", async () => {
    mockHomeSections({
      libraries: [],
      recommended: [],
      continueWatching: [{
        id: "episode-only",
        title: "不应进入轮播的单集",
        itemType: "EPISODE",
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

    expect(container.querySelector('[aria-label="精选媒体轮播"] .lux-hero-title-text')?.textContent)
      .toBe("你的私人影院");
    expect(container.querySelector('[aria-label="精选媒体轮播"]')?.textContent)
      .not.toContain("不应进入轮播的单集");
  });

  it("starts the highlighted movie from the carousel instead of opening its detail page", async () => {
    mockHomeSections({
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

    await waitForHomeUpdate(() => expect(container.querySelector<HTMLAnchorElement>(".lux-hero-actions a.lux-button-primary")?.getAttribute("href"))
      .toBe("/watch/movie-1"));
  });

  it("applies the smallest title size while keeping the complete title text available", async () => {
    const title = "FC2-4916281 脸）强忍着因为嘘息而即将失禁，但在猛烈的冲击下不停地溢出";
    mockHomeSections({
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

    await waitForHomeUpdate(() => expect(container.querySelector(".lux-hero-title-text")?.textContent).toBe(title));
    const heroTitle = container.querySelector(".lux-hero-title");
    expect(heroTitle?.classList.contains("lux-hero-title--small")).toBe(true);
    expect(heroTitle?.querySelector(".lux-hero-title-text")?.textContent).toBe(title);
  });

  it("uses an available media logo in the carousel title area", async () => {
    mockHomeSections({
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

    await waitForHomeUpdate(() => expect(container.querySelector<HTMLImageElement>(".lux-hero-logo")?.getAttribute("src"))
      .toBe("/api/v1/items/featured-1/images/logo?tag=logo-tag"));
    expect(container.querySelector<HTMLImageElement>(".lux-hero-logo")?.getAttribute("decoding")).toBe("async");
    expect(container.querySelector(".lux-hero-title")?.textContent).toBe("");
    expect(container.querySelector(".lux-hero-title")?.querySelector("img")?.getAttribute("alt"))
      .toBe("精选电影");
  });

  it("restores only cached carousel recommendations while sections reload", async () => {
    const response = { recommended: [{ id: "featured-cached", title: "缓存轮播", itemType: "MOVIE" }] };
    const home = vi.spyOn(api, "homeCarousel").mockResolvedValueOnce(response);
    vi.spyOn(api, "homeLibraries").mockResolvedValue({
      libraries: [{ id: "library-1", name: "电影库", kind: "MOVIE" }],
    });
    vi.spyOn(api, "homeContinueWatching").mockResolvedValue({ items: [], total: 0 });
    vi.spyOn(api, "homeLibrariesLatest").mockResolvedValue({
      libraries: [{ libraryId: "library-1", items: [] }],
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const firstQueryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(
        <QueryClientProvider client={firstQueryClient}>
          <MemoryRouter>
            <HomePage user={user} />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    await waitForHomeUpdate(() => expect(container.querySelector(".lux-hero-title-text")?.textContent).toContain("缓存轮播"));
    const cachedCarousel = JSON.parse(sessionStorage.getItem("lux.home-carousel.v2:user-1") ?? "{}");
    expect(Object.keys(cachedCarousel.data ?? {})).toEqual(["recommended"]);

    act(() => root?.unmount());
    root = undefined;

    let resolveRefresh: ((value: typeof response) => void) | undefined;
    home.mockReturnValueOnce(new Promise((resolve) => { resolveRefresh = resolve; }));
    const secondQueryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    root = createRoot(container);
    await act(async () => {
      root?.render(
        <QueryClientProvider client={secondQueryClient}>
          <MemoryRouter>
            <HomePage user={user} />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });

    await waitForHomeUpdate(() => expect(container.querySelector(".lux-hero-title-text")?.textContent).toContain("缓存轮播"));
    expect(container.querySelector(".lux-skeleton-page")).toBeNull();

    await act(async () => {
      resolveRefresh?.(response);
    });
  });

  it("prefetches the first library page when a homepage library is focused", async () => {
    mockHomeSections({
      libraries: [{ id: "library-1", name: "电影库", kind: "MOVIE", latest: [] }],
      recommended: [],
      continueWatching: [],
    });
    vi.spyOn(api, "libraryItems").mockResolvedValue({ items: [], page: 1, pageSize: 24, total: 0 });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const prefetch = vi.spyOn(queryClient, "prefetchInfiniteQuery");

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

    const libraryLink = container.querySelector<HTMLAnchorElement>(".lux-library-card-copy-link");
    expect(libraryLink).not.toBeNull();
    await act(async () => {
      libraryLink?.focus();
    });

    expect(prefetch).toHaveBeenCalledWith(expect.objectContaining({
      queryKey: queryKeys.library("library-1", 1, "MOVIE,SERIES", "Name", "Ascending", "all"),
      initialPageParam: 1,
    }));
  });

  it("reuses hero slides when only the latest-library shelf changes", async () => {
    const latestRequest = mockHomeSections({
      libraries: [{ id: "library-1", name: "电影库", kind: "MOVIE", latest: [] }],
      recommended: [{ id: "hero-1", title: "精选影片", itemType: "MOVIE" }],
      continueWatching: [],
    });
    heroSlidesTracker.mockClear();
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
      await waitForHomeUpdate(() => expect(container.querySelector(".lux-hero-title-text")?.textContent)
        .toContain("精选影片"));
      await waitForHomeUpdate(() => expect(latestRequest).toHaveBeenCalledWith(["library-1"], expect.anything()));
      await waitForHomeUpdate(() => {
        expect(queryClient.getQueryData(queryKeys.homeLatestLibraries(["library-1"]))).toEqual({
          libraries: [{ libraryId: "library-1", items: [] }],
        });
        expect(queryClient.getQueryState(queryKeys.homeCarousel)?.fetchStatus).toBe("idle");
        expect(queryClient.getQueryState(queryKeys.homeContinueWatching)?.fetchStatus).toBe("idle");
        expect(queryClient.getQueryState(queryKeys.homeLatestLibraries(["library-1"]))?.fetchStatus).toBe("idle");
      });
    });
    heroSlidesTracker.mockClear();

    await act(async () => {
      queryClient.setQueryData(queryKeys.homeLatestLibraries(["library-1"]), {
        libraries: [{
          libraryId: "library-1",
          items: [{ id: "latest-2", title: "新到影片", itemType: "MOVIE" }],
        }],
      });
    });

    await act(async () => {
      await waitForHomeUpdate(() => expect(container.querySelector('[aria-label="最新电影库"]')?.textContent)
        .toContain("新到影片"));
    });
    expect(heroSlidesTracker).not.toHaveBeenCalled();
  });
});
