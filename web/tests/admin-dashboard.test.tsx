// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AdminDashboardPage } from "../src/features/admin/AdminDashboardPage";
import { api } from "../src/lib/api/client";
import { queryKeys, queryRefreshIntervals } from "../src/lib/api/query-keys";
import type { AdminDashboard, AdminSettings } from "../src/lib/api/types";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

const dashboard: AdminDashboard = {
  server: { name: "客厅 Lux", version: "0.1.0", commit: "abc1234", schemaVersion: 37 },
  health: {
    status: "ok",
    schemaVersion: 37,
    database: { status: "ok", journalMode: "wal", writable: true },
    config: { available: true, writable: true },
    ffprobe: { available: true },
    tmdb: { configured: true },
    jobs: { scanRunning: 1, scanFailed: 0, metadataReidentifyRunning: 0 },
    libraries: [{ id: "library-1", name: "电影库", isEnabled: true, rootCount: 1, availableRootCount: 1, writableRootCount: 1 }],
  },
  nowPlaying: [{
    id: "playback-1",
    userId: "user-1",
    userName: "pdz",
    itemId: "item-1",
    title: "爱情情节顶红",
    seriesId: "series-1",
    seriesTitle: "九门",
    itemType: "EPISODE",
    productionYear: 2025,
    parentIndexNumber: 1,
    indexNumber: 9,
    posterAvailable: true,
    positionTicks: 1800000000,
    durationTicks: 54000000000,
    state: "PLAYING",
    isPaused: false,
    lastEventAt: 1_700_000_000,
    client: "VidHub",
    clientVersion: "3.0.2",
    deviceId: "iphone",
    deviceName: "iPhone",
    deviceType: "Phone",
    remoteIp: "192.0.2.10",
    playSessionId: "session-1",
    source: {
      id: "source-1",
      qualityLabel: "4K HEVC",
      container: "MKV",
      bitrate: 4000000,
      video: { codec: "HEVC", title: "4K HDR" },
      audio: { codec: "AAC", language: "zh-CN", title: "立体声" },
    },
  }],
  activity: [
    { id: "activity-login", userName: "admin", eventType: "AUTH_LOGIN", createdAt: 1_700_000_500 },
    { id: "activity-1", userName: "pdz", eventType: "PLAYBACK_STARTED", targetId: "item-1", metadata: { deviceName: "iPhone" }, createdAt: 1_700_000_000 },
    { id: "activity-2", userName: "n anzi", eventType: "PLAYBACK_PAUSED", targetId: "item-2", createdAt: 1_699_999_000 },
    { id: "activity-3", userName: "n anzi", eventType: "PLAYBACK_STOPPED", targetId: "item-3", createdAt: 1_699_998_000 },
    { id: "activity-4", userName: "viewer 4", eventType: "PLAYBACK_STARTED", targetId: "item-4", createdAt: 1_699_997_000 },
    { id: "activity-5", userName: "viewer 5", eventType: "PLAYBACK_STARTED", targetId: "item-5", createdAt: 1_699_996_000 },
    { id: "activity-6", userName: "viewer 6", eventType: "PLAYBACK_STARTED", targetId: "item-6", createdAt: 1_699_995_000 },
    { id: "activity-7", userName: "viewer 7", eventType: "PLAYBACK_STARTED", targetId: "item-7", createdAt: 1_699_994_000 },
    { id: "activity-8", userName: "viewer 8", eventType: "PLAYBACK_STARTED", targetId: "item-8", createdAt: 1_699_993_000 },
    { id: "activity-9", userName: "viewer 9", eventType: "PLAYBACK_STARTED", targetId: "item-9", createdAt: 1_699_992_000 },
    { id: "activity-10", userName: "viewer 10", eventType: "PLAYBACK_STARTED", targetId: "item-10", createdAt: 1_699_991_000 },
    { id: "activity-11", userName: "viewer 11", eventType: "PLAYBACK_STARTED", targetId: "item-11", createdAt: 1_699_990_000 },
    { id: "activity-12", userName: "viewer 12", eventType: "PLAYBACK_STARTED", targetId: "item-12", createdAt: 1_699_989_000 },
  ],
};

const settings: AdminSettings = {
  serverName: "客厅 Lux",
  resumePlayedPercent: 90,
  resumeMinTicks: 1_200_000_000,
  mediaStrategy: {
    metadataLanguage: "zh-CN",
    imageLanguage: "zh-CN",
    region: "CN",
    scraperId: null,
    applyScope: "NEW_CONTENT",
    images: { poster: true, artwork: false, banner: false, logo: true, thumbnail: true, disc: false, wallpaper: false, maxBackdropCount: 1, minDownloadWidth: 1280 },
    subtitles: { autoDownload: false, languages: ["zh-CN"], forcedOnly: false, hearingImpaired: false },
  },
};

describe("AdminDashboardPage", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("renders server identity, rich playback cards, and account activity", async () => {
    const load = vi.spyOn(api, "adminDashboard").mockResolvedValue(dashboard);
    const update = vi.spyOn(api, "updateAdminSettings").mockResolvedValue(settings);
    vi.spyOn(api, "adminHealth").mockResolvedValue(dashboard.health);
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [] });
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    act(() => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <AdminDashboardPage />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("客厅 Lux"));
    });
    expect(queryClient.getQueryCache().find({ queryKey: queryKeys.adminDashboard })?.options.refetchInterval)
      .toBe(queryRefreshIntervals.liveDashboard);
    expect(load).toHaveBeenCalledOnce();
    expect(container.textContent).toContain("v0.1.0");
    expect(container.querySelector(".lux-admin-stat-grid")).toBeNull();
    expect(container.querySelectorAll(".lux-admin-stat")).toHaveLength(0);
    expect(container.textContent).toContain("爱情情节顶红");
    const playbackCard = container.querySelector(".lux-now-playing-card");
    expect(playbackCard?.querySelector(".lux-now-playing-title")?.textContent).toBe("九门");
    expect(playbackCard?.querySelector(".lux-now-playing-title")?.getAttribute("href")).toBe("/items/series-1");
    expect(playbackCard?.querySelector(".lux-now-playing-subtitle")?.textContent).toBe("S01E09 · 爱情情节顶红");
    expect(playbackCard?.querySelector(".lux-now-playing-heading > .lux-now-playing-subtitle")).not.toBeNull();
    expect(playbackCard?.querySelector(".lux-now-playing-heading-copy > .lux-now-playing-subtitle")).toBeNull();
    expect(container.textContent).toContain("VidHub");
    expect(container.textContent).toContain("v3.0.2");
    const accountEntries = [...container.querySelectorAll(".lux-now-playing-account-entry")]
      .map((entry) => entry.textContent);
    expect(accountEntries).toEqual(["用户pdz", "设备iPhone", "客户端VidHubv3.0.2"]);
    expect(container.textContent).toContain("4K HEVC");
    expect(container.textContent).toContain("HEVC");
    expect(container.textContent).toContain("AAC · zh-CN");
    expect(container.textContent).toContain("192.0.2.10");
    expect(container.textContent).not.toContain("NOW PLAYING");
    expect(container.textContent).not.toContain("IP 地址");
    expect(container.textContent).not.toContain("IP 归属地");
    expect(container.querySelector(".lux-now-playing-kicker")).toBeNull();
    expect(container.querySelector(".lux-now-playing-network")).not.toBeNull();
    expect(container.querySelector('[role="group"][aria-label="IP 地址"]')).not.toBeNull();
    expect(container.querySelector('[role="group"][aria-label="IP 归属地"]')).not.toBeNull();
    expect(container.querySelector(".lux-now-playing-account")).not.toBeNull();
    expect(container.querySelector(".lux-now-playing-facts")).not.toBeNull();
    expect(container.querySelectorAll(".lux-now-playing-fact")).toHaveLength(3);
    expect(container.querySelectorAll(".lux-now-playing-fact")[0]?.textContent).toBe("来源：4K HEVC · MKV · 4.0 Mbps");
    expect(container.querySelectorAll(".lux-now-playing-fact")[1]?.textContent).toBe("视频：HEVC · 4K HDR");
    expect(container.querySelectorAll(".lux-now-playing-fact")[2]?.textContent).toBe("音频：AAC · zh-CN · 立体声");
    expect(container.querySelectorAll(".lux-now-playing-placeholder")).toHaveLength(1);
    expect(container.textContent).toContain("开始播放");
    expect(container.textContent).toContain("暂停播放");
    expect(container.textContent).toContain("停止播放");
    expect(container.textContent).toContain("最近 10 条");
    expect(container.textContent).not.toContain("登录");
    expect(container.textContent).not.toContain("viewer 11");
    expect(container.querySelectorAll(".lux-admin-activity-row")).toHaveLength(10);

    const input = container.querySelector<HTMLInputElement>("input[name='serverName']");
    expect(input?.value).toBe("客厅 Lux");
    await act(async () => {
      if (!input) throw new Error("server name input missing");
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(input, "书房 Lux");
      input.dispatchEvent(new Event("input", { bubbles: true }));
      input.dispatchEvent(new Event("change", { bubbles: true }));
      container.querySelector<HTMLButtonElement>("button[type='submit']")?.click();
      await vi.waitFor(() => expect(update).toHaveBeenCalledWith({ serverName: "书房 Lux" }));
    });
  });

  it("shows a movie kind once instead of repeating it across card metadata", async () => {
    const movieDashboard: AdminDashboard = {
      ...dashboard,
      nowPlaying: [{
        ...dashboard.nowPlaying[0],
        id: "playback-movie",
        title: "一毛",
        originalTitle: null,
        seriesId: null,
        seriesTitle: null,
        itemType: "MOVIE",
        productionYear: 2019,
        parentIndexNumber: null,
        indexNumber: null,
      }],
    };
    vi.spyOn(api, "adminDashboard").mockResolvedValue(movieDashboard);
    vi.spyOn(api, "updateAdminSettings").mockResolvedValue(settings);
    vi.spyOn(api, "adminHealth").mockResolvedValue(movieDashboard.health);
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [] });
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    act(() => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <AdminDashboardPage />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("一毛"));
    });

    const movieCard = container.querySelector(".lux-now-playing-card");
    expect(movieCard?.querySelector(".lux-now-playing-title")?.textContent).toBe("一毛");
    expect(movieCard?.querySelector(".lux-now-playing-subtitle")).toBeNull();
  });
});
