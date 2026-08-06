// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AdminDashboardPage } from "../src/features/admin/AdminDashboardPage";
import { api } from "../src/lib/api/client";
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
    client: "VidHub 3.0.2",
    deviceId: "iphone",
    deviceName: "iPhone",
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
    { id: "activity-1", userName: "pdz", eventType: "PLAYBACK_STARTED", targetId: "item-1", metadata: { deviceName: "iPhone" }, createdAt: 1_700_000_000 },
    { id: "activity-2", userName: "admin", eventType: "AUTH_LOGIN", createdAt: 1_699_999_000 },
    { id: "activity-3", userName: "n anzi", eventType: "PLAYBACK_STOPPED", targetId: "item-2", createdAt: 1_699_998_000 },
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
    expect(load).toHaveBeenCalledOnce();
    expect(container.textContent).toContain("v0.1.0");
    expect(container.textContent).toContain("爱情情节顶红");
    expect(container.textContent).toContain("S1 · E9");
    expect(container.textContent).toContain("VidHub 3.0.2");
    expect(container.textContent).toContain("4K HEVC");
    expect(container.textContent).toContain("HEVC");
    expect(container.textContent).toContain("AAC · zh-CN");
    expect(container.textContent).toContain("开始播放");
    expect(container.textContent).toContain("登录");
    expect(container.textContent).toContain("停止播放");

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
});
