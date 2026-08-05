// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { MediaDetailPage } from "../src/features/detail/MediaDetailPage";
import { PlayerPage } from "../src/features/player/PlayerPage";
import { api } from "../src/lib/api/client";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("MediaDetailPage series hierarchy", () => {
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

  it("shows portrait season cards on a series detail", async () => {
    vi.spyOn(api, "item").mockResolvedValue({
      id: "series-1",
      title: "示例剧集",
      itemType: "SERIES",
      rating: 7.6,
      ratingSource: "TMDb",
      mediaSources: [],
    });
    vi.spyOn(api, "playback").mockResolvedValue({});
    vi.spyOn(api, "children").mockImplementation(async (_itemId, options) => ({
      items: options?.itemType === "SEASON"
        ? [{ id: "season-1", title: "第一季", itemType: "SEASON", imageTags: { poster: "season-poster" } }]
        : [{ id: "episode-1", title: "第一集", itemType: "EPISODE" }],
      total: 1,
      page: 1,
      pageSize: 60,
    }));

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/items/series-1"]}>
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

    expect(container.querySelector(".lux-detail-copy .lux-eyebrow")).toBeNull();
    expect(container.querySelector(".lux-detail-poster .lux-rating")).toBeNull();
    expect(container.querySelector(".lux-season-rail")?.textContent).toContain("第一季");
    expect(container.querySelector(".lux-season-card img")?.getAttribute("src"))
      .toBe("/api/v1/items/season-1/images/poster");
    expect(container.querySelector(".lux-season-tabs")).toBeNull();
    expect(container.querySelector(".lux-episode-list")).toBeNull();
  });

  it("shows the available series metadata in the detail hero", async () => {
    vi.spyOn(api, "item").mockResolvedValue({
      id: "series-1",
      title: "示例剧集",
      originalTitle: "Rick and Morty",
      itemType: "SERIES",
      premiereDate: "2013-12-02",
      lastAirDate: "2025-05-25",
      status: "Ended",
      originalLanguage: "en",
      rating: 8.68,
      ratingSource: "TMDb",
      providerIds: { tmdb: "60625" },
      seasonCount: 7,
      episodeCount: 91,
      mediaSources: [{ id: "source-1", sourceKind: "LOCAL_FILE", isDefault: true }],
    });
    vi.spyOn(api, "playback").mockResolvedValue({});
    vi.spyOn(api, "children").mockResolvedValue({ items: [], total: 0, page: 1, pageSize: 60 });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/items/series-1"]}>
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

    expect(container.querySelector(".lux-detail-title-row h1")?.textContent).toBe("示例剧集");
    expect(container.querySelector(".lux-detail-original-title")?.textContent).toBe("Rick and Morty");
    expect(container.querySelector(".lux-detail-meta")?.textContent).toContain("首播 2013-12-02");
    expect(container.querySelector(".lux-detail-meta")?.textContent).toContain("7 季");
    expect(container.querySelector(".lux-detail-meta")?.textContent).toContain("91 集");
    expect(container.querySelector(".lux-detail-meta")?.textContent).toContain("TMDb 60625");
    expect(container.querySelector(".lux-detail-meta")?.textContent).toContain("评分 8.7");
    expect(container.querySelector(".lux-detail-poster .lux-rating")).toBeNull();
    expect(container.querySelector(".lux-media-info-extra")?.textContent).toContain("最后播出2025-05-25");
    expect(container.querySelector(".lux-media-info-extra")?.textContent).toContain("状态Ended");
    expect(container.querySelector(".lux-media-info-extra")?.textContent).toContain("原始语言英语");
    expect(container.querySelector(".lux-season-rail")).not.toBeNull();
  });

  it("shows landscape episode rows on a season detail", async () => {
    vi.spyOn(api, "item").mockImplementation(async (itemId) => itemId === "season-1"
      ? {
        id: "season-1",
        title: "第三季",
        itemType: "SEASON",
        parentId: "series-1",
        seriesId: "series-1",
        parentIndexNumber: 3,
        imageTags: { poster: "season-poster" },
        mediaSources: [],
      }
      : { id: "series-1", title: "示例剧集", itemType: "SERIES", mediaSources: [] });
    vi.spyOn(api, "playback").mockResolvedValue({});
    vi.spyOn(api, "children").mockResolvedValue({
      items: [
        { id: "episode-1", title: "第一集", itemType: "EPISODE", indexNumber: 1, imageTags: { fanart: "episode-fanart" } },
        { id: "episode-2", title: "第二集", itemType: "EPISODE", indexNumber: 2, imageTags: { fanart: "episode-fanart-2" } },
      ],
      total: 2,
      page: 1,
      pageSize: 60,
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/items/season-1"]}>
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

    expect(container.querySelector(".lux-detail-page-season")).not.toBeNull();
    expect(container.querySelector(".lux-detail-title-row h1")?.textContent).toBe("示例剧集");
    expect(container.querySelector(".lux-detail-subtitle")?.textContent).toContain("第 3 季");
    expect(container.querySelectorAll(".lux-season-episode-row")).toHaveLength(2);
    expect(container.querySelector(".lux-season-episode-row strong")?.textContent).toBe("1. 第一集");
    expect(container.querySelector(".lux-season-episode-thumb img")?.getAttribute("src"))
      .toBe("/api/v1/items/episode-1/images/fanart");
  });

  it("shows a landscape episode hero and more episodes from its season", async () => {
    vi.spyOn(api, "item").mockImplementation(async (itemId) => {
      if (itemId === "episode-3") {
        return {
          id: "episode-3",
          title: "第三集标题",
          itemType: "EPISODE",
          parentId: "season-1",
          seriesId: "series-1",
          parentIndexNumber: 3,
          indexNumber: 3,
          imageTags: { fanart: "episode-fanart-3" },
          mediaSources: [],
        };
      }
      return { id: "series-1", title: "示例剧集", itemType: "SERIES", mediaSources: [] };
    });
    vi.spyOn(api, "playback").mockResolvedValue({});
    vi.spyOn(api, "children").mockResolvedValue({
      items: [
        { id: "episode-2", title: "第二集", itemType: "EPISODE", indexNumber: 2, imageTags: { fanart: "episode-fanart-2" } },
        { id: "episode-3", title: "第三集标题", itemType: "EPISODE", indexNumber: 3, imageTags: { fanart: "episode-fanart-3" } },
      ],
      total: 2,
      page: 1,
      pageSize: 60,
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/items/episode-3"]}>
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

    expect(container.querySelector(".lux-detail-page-episode")).not.toBeNull();
    expect(container.querySelector(".lux-detail-poster.is-landscape")).not.toBeNull();
    expect(container.querySelector(".lux-detail-poster.is-landscape img")?.getAttribute("src"))
      .toBe("/api/v1/items/episode-3/images/fanart");
    expect(container.querySelector(".lux-detail-title-row h1")?.textContent).toBe("示例剧集");
    expect(container.querySelector(".lux-detail-subtitle")?.textContent).toContain("S3.E3");
    expect(container.querySelector(".lux-episode-rail")?.textContent).toContain("更多来自第 3 季");
    expect(container.querySelectorAll(".lux-episode-card")).toHaveLength(1);
    expect(container.querySelector(".lux-episode-card img")?.getAttribute("src"))
      .toBe("/api/v1/items/episode-2/images/fanart");
  });

  it("uses the server playback state for the watched indicator", async () => {
    vi.spyOn(api, "item").mockResolvedValue({
      id: "movie-1",
      title: "示例电影",
      itemType: "MOVIE",
      rating: 8.2,
      ratingSource: "TMDb",
      mediaSources: [],
    });
    vi.spyOn(api, "playback").mockResolvedValue({ isPlayed: true });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/items/movie-1"]}>
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

    const watched = container.querySelector(".lux-detail-watched-status");
    expect(watched?.classList.contains("is-played")).toBe(true);
    expect(watched?.getAttribute("aria-label")).toBe("已看");
    expect(container.querySelector(".lux-detail-poster .lux-rating")).toBeNull();

    const actionItems = [...(container.querySelector(".lux-hero-actions")?.children ?? [])];
    expect(actionItems[3]?.classList.contains("lux-media-actions")).toBe(true);
    expect(actionItems[3]?.querySelector(".lux-media-actions-trigger")).not.toBeNull();
  });

  it("shows identified actors with local portraits or initial placeholders", async () => {
    vi.spyOn(api, "item").mockResolvedValue({
      id: "movie-1",
      title: "示例电影",
      itemType: "MOVIE",
      actors: [
        { id: "9", name: "演员甲", character: "角色甲", imageUrl: "/api/v1/people/9/image" },
        { id: "10", name: "演员乙", character: "角色乙", imageUrl: null },
      ],
      mediaSources: [],
    });
    vi.spyOn(api, "playback").mockResolvedValue({});

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/items/movie-1"]}>
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

    const cast = container.querySelector(".lux-media-cast");
    expect(cast?.querySelector("h2")?.textContent).toBe("演职人员");
    expect(cast?.querySelectorAll(".lux-media-cast-card")).toHaveLength(2);
    expect(cast?.querySelector<HTMLImageElement>(".lux-media-cast-avatar img")?.src)
      .toContain("/api/v1/people/9/image");
    expect(cast?.textContent).toContain("演员甲");
    expect(cast?.textContent).toContain("角色甲");
    expect(cast?.querySelector(".lux-media-cast-initial")?.textContent).toBe("演");
  });

  it("opens the complete overview from the three-line summary", async () => {
    const overview = "这是一段足够长的剧情简介，用来验证详情页只展示三行内容，并在末尾提供更多入口。".repeat(5);
    vi.spyOn(api, "item").mockResolvedValue({
      id: "movie-1",
      title: "示例电影",
      itemType: "MOVIE",
      overview,
      mediaSources: [],
    });
    vi.spyOn(api, "playback").mockResolvedValue({});

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/items/movie-1"]}>
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

    const more = container.querySelector<HTMLButtonElement>(".lux-detail-overview-more");
    expect(more?.textContent).toBe("更多");
    expect(more?.classList.contains("is-underlined")).toBe(true);

    await act(async () => more?.click());

    expect(container.querySelector('[role="dialog"]')?.textContent).toContain(overview);
    const close = container.querySelector<HTMLButtonElement>('[role="dialog"] button[aria-label="关闭详细信息"]');
    expect(close).toBeDefined();

    await act(async () => close?.click());
    expect(container.querySelector('[role="dialog"]')).toBeNull();
  });

  it("lets a movie or episode detail choose the source passed to the player", async () => {
    vi.spyOn(api, "item").mockResolvedValue({
      id: "episode-1",
      title: "第一集",
      itemType: "EPISODE",
      mediaSources: [
        {
          id: "source-sdr",
          qualityLabel: "1080p",
          container: "mkv",
          isDefault: true,
          streams: [{ index: 0, type: "VIDEO", codec: "h264", details: { VideoRangeType: "SDR", BitDepth: 8 } }],
        },
        {
          id: "source-hdr",
          qualityLabel: "2160p",
          container: "mkv",
          isDefault: false,
          streams: [{ index: 0, type: "VIDEO", codec: "hevc", details: { VideoRangeType: "HDR10", BitDepth: 10 } }],
        },
      ],
    });
    vi.spyOn(api, "playback").mockResolvedValue({});

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/items/episode-1"]}>
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

    const sourceSelect = container.querySelector<HTMLButtonElement>(".lux-source-selector [role=\"combobox\"]");
    expect(sourceSelect?.textContent).toContain("1080p · H.264 · SDR · 8-bit");
    expect(sourceSelect?.getAttribute("aria-expanded")).toBe("false");

    await act(async () => sourceSelect?.click());

    expect(sourceSelect?.getAttribute("aria-expanded")).toBe("true");
    const hdrOption = [...document.querySelectorAll<HTMLButtonElement>("[role=\"option\"]")]
      .find((button) => button.textContent?.includes("2160p · HEVC · HDR10 · 10-bit"));
    expect(hdrOption).toBeDefined();
    await act(async () => hdrOption?.click());

    expect(sourceSelect?.textContent).toContain("2160p · HEVC · HDR10 · 10-bit");
    expect(sourceSelect?.getAttribute("aria-expanded")).toBe("false");
    expect(container.querySelector<HTMLAnchorElement>("a.lux-button-primary")?.getAttribute("href"))
      .toBe("/watch/episode-1?sourceId=source-hdr");
  });

  it("shows source metadata and detailed video, audio, and subtitle tracks", async () => {
    vi.spyOn(api, "item").mockResolvedValue({
      id: "movie-1",
      title: "示例电影",
      itemType: "MOVIE",
      mediaSources: [{
        id: "source-1",
        sourceKind: "STRM_URL",
        container: "mkv",
        size: 1_573_860_454,
        bitrate: 2_392_049,
        durationTicks: 52_636_380_000,
        externalUrl: "https://example.invalid/video.mkv",
        isDefault: true,
        streams: [
          {
            index: 0,
            type: "VIDEO",
            codec: "h264",
            title: "1080p H264",
            isDefault: true,
            details: {
              Width: 1920,
              Height: 1080,
              Profile: "High",
              RealFrameRate: 30,
              BitDepth: 8,
              PixelFormat: "yuv420p",
            },
          },
          {
            index: 1,
            type: "AUDIO",
            codec: "aac",
            title: "AAC stereo (默认)",
            language: "chi",
            isDefault: true,
            details: {
              ChannelLayout: "stereo",
              Channels: 2,
              SampleRate: 44100,
              BitRate: 192000,
            },
          },
          {
            index: 2,
            type: "SUBTITLE",
            codec: "subrip",
            language: "chi",
            isExternal: false,
            isForced: false,
            details: {},
          },
        ],
      }],
    });
    vi.spyOn(api, "playback").mockResolvedValue({});

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/items/movie-1"]}>
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

    const details = container.querySelector(".lux-media-info");
    expect(details?.textContent).toContain("其它信息");
    expect(details?.textContent).toContain("媒体信息");
    expect(details?.textContent).toContain("https://example.invalid/video.mkv");
    expect(details?.textContent).toContain("1920 × 1080");
    expect(details?.textContent).toContain("H264");
    expect(details?.textContent).toContain("stereo");
    expect(details?.textContent).toContain("中文字幕");
  });

  it("places the title on the line below an available media logo", async () => {
    vi.spyOn(api, "item").mockResolvedValue({
      id: "movie-1",
      title: "示例电影",
      itemType: "MOVIE",
      mediaSources: [],
    });
    vi.mocked(api.itemImages).mockResolvedValue({
      images: [{
        id: "logo-1",
        itemId: "movie-1",
        imageType: "LOGO",
        imageIndex: 0,
        url: "/api/v1/items/movie-1/images/logo",
      }],
    });
    vi.spyOn(api, "playback").mockResolvedValue({});

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/items/movie-1"]}>
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

    const titleRow = container.querySelector(".lux-detail-title-row");
    expect(titleRow?.querySelector<HTMLImageElement>(".lux-detail-logo")?.getAttribute("src"))
      .toBe("/api/v1/items/movie-1/images/logo");
    expect(titleRow?.querySelector("h1")?.textContent).toBe("示例电影");
    expect(titleRow?.children[1]?.tagName).toBe("H1");
  });

  it("plays the source selected in the detail URL", async () => {
    vi.spyOn(api, "item").mockResolvedValue({
      id: "episode-1",
      title: "第一集",
      itemType: "EPISODE",
      mediaSources: [
        { id: "source-sdr", qualityLabel: "1080p SDR", isDefault: true },
        { id: "source-hdr", qualityLabel: "2160p HDR", isDefault: false },
      ],
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/watch/episode-1?sourceId=source-hdr"]}>
            <Routes>
              <Route path="watch/:itemId" element={<PlayerPage />} />
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(container.querySelector<HTMLVideoElement>("video")?.getAttribute("src"))
      .toBe("/api/v1/items/episode-1/stream?sourceId=source-hdr");
  });

  it("passes a strm source directly to the browser", async () => {
    vi.spyOn(api, "item").mockResolvedValue({
      id: "movie-1",
      title: "示例电影",
      itemType: "MOVIE",
      mediaSources: [{
        id: "source-strm",
        sourceKind: "STRM_URL",
        externalUrl: "https://example.invalid/video.mkv",
        isDefault: true,
      }],
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/watch/movie-1"]}>
            <Routes>
              <Route path="watch/:itemId" element={<PlayerPage />} />
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(container.querySelector<HTMLVideoElement>("video")?.getAttribute("src"))
      .toBe("https://example.invalid/video.mkv");
  });

  it("shows a clear message when the browser cannot play the source", async () => {
    vi.spyOn(api, "item").mockResolvedValue({
      id: "movie-1",
      title: "示例电影",
      itemType: "MOVIE",
      mediaSources: [{ id: "source-local", container: "mkv", isDefault: true }],
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/watch/movie-1"]}>
            <Routes>
              <Route path="watch/:itemId" element={<PlayerPage />} />
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    await act(async () => {
      container.querySelector<HTMLVideoElement>("video")?.dispatchEvent(new Event("error"));
    });

    expect(container.querySelector(".lux-player-error")?.textContent)
      .toContain("浏览器无法播放这个媒体源");
  });
});
