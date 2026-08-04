// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter } from "react-router-dom";
import { AdminLibrariesPage } from "../src/features/admin/AdminLibrariesPage";
import { api } from "../src/lib/api/client";
import type { AdminPlugin } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const library = {
  id: "library-1",
  name: "01每日更新",
  kind: "MIXED",
  coverImageUrl: "/covers/daily-updates.jpg",
  itemCount: 12,
  isEnabled: true,
  realtimeWatchEnabled: true,
  roots: [{
    id: "root-1",
    libraryId: "library-1",
    canonicalPath: "/media/strm/video/每日更新",
    displayPath: "/media/strm/video/每日更新",
    isAvailable: true,
    isWritable: true,
  }],
};

const configuredScraper: AdminPlugin = {
  id: "tmdb",
  name: "TMDb 元数据插件",
  description: "通过 TMDb 补全媒体元数据和图片。",
  category: "SCRAPER",
  version: "1.0.0",
  runtime: "builtin",
  capabilities: ["metadata"],
  status: "READY",
  running: true,
  lastError: null,
  installed: true,
  enabled: true,
  configured: true,
  available: true,
  unavailableReason: null,
  configurable: true,
  configFields: [],
  configSource: "BUILT_IN",
};

describe("AdminLibrariesPage library cards", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [library] });
    vi.spyOn(api, "adminPlugins").mockResolvedValue({ plugins: [configuredScraper] });
    vi.spyOn(api, "adminSettings").mockResolvedValue({
      resumePlayedPercent: 90,
      resumeMinTicks: 1_200_000_000,
      mediaStrategy: {
        metadataLanguage: "zh-CN",
        imageLanguage: "zh-CN",
        region: "CN",
        scraperId: null,
        applyScope: "NEW_CONTENT",
        images: {
          poster: true,
          artwork: false,
          banner: false,
          logo: true,
          thumbnail: true,
          disc: false,
          wallpaper: false,
          maxBackdropCount: 1,
          minDownloadWidth: 1280,
        },
        subtitles: {
          autoDownload: false,
          languages: ["zh-CN"],
          forcedOnly: false,
          hearingImpaired: false,
        },
      },
    });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  async function renderPage() {
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <AdminLibrariesPage />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
  }

  it("renders a library card with its cover, type, and root path", async () => {
    await renderPage();

    expect(container.querySelector(".lux-admin-library-grid")).toBeTruthy();
    expect(container.querySelector(".lux-admin-library-cover")?.getAttribute("src")).toBe("/covers/daily-updates.jpg");
    expect(container.textContent).toContain("01每日更新");
    expect(container.textContent).toContain("混合内容");
    expect(container.textContent).toContain("/media/strm/video/每日更新");
  });

  it("opens the library actions menu from the card overflow button", async () => {
    await renderPage();

    const menuButton = container.querySelector<HTMLButtonElement>("[aria-label='打开 01每日更新 操作菜单']");
    expect(menuButton).toBeTruthy();

    await act(async () => menuButton?.click());

    expect(container.querySelector('[role="menu"]')?.textContent).toContain("编辑");
    expect(container.querySelector('[role="menu"]')?.textContent).toContain("扫描媒体库文件");
  });

  it("opens the edit dialog from the library actions menu", async () => {
    await renderPage();

    await act(async () => {
      container.querySelector<HTMLButtonElement>("[aria-label='打开 01每日更新 操作菜单']")?.click();
    });
    const editAction = [...container.querySelectorAll<HTMLButtonElement>('[role="menu"] button')]
      .find((button) => button.textContent?.includes("编辑"));
    expect(editAction).toBeTruthy();

    await act(async () => editAction?.click());

    expect(container.querySelector('[role="dialog"]')?.textContent).toContain("01每日更新");
    expect(container.querySelector<HTMLInputElement>('[aria-label="01每日更新 媒体库名称"]')?.value).toBe("01每日更新");
  });

  it("lists only configured scrapers without a local-only scraper option", async () => {
    await renderPage();

    await act(async () => {
      container.querySelector<HTMLButtonElement>("[aria-label='打开 01每日更新 操作菜单']")?.click();
    });
    await act(async () => {
      [...container.querySelectorAll<HTMLButtonElement>("[role='menu'] button")]
        .find((button) => button.textContent?.includes("编辑"))
        ?.click();
    });

    expect(container.textContent).not.toContain("仅使用本地元数据");
    const scraperTrigger = container.querySelector<HTMLButtonElement>("[aria-label='刮削器']");
    expect(scraperTrigger).toBeTruthy();

    await act(async () => scraperTrigger?.click());

    expect(document.body.textContent).toContain("TMDb 元数据插件");
    expect(document.body.textContent).not.toContain("仅使用本地元数据");
  });

  it("shows global image and subtitle defaults in the strategy view", async () => {
    await renderPage();

    await act(async () => {
      [...container.querySelectorAll<HTMLButtonElement>("[role='tab']")]
        .find((button) => button.textContent?.includes("高级"))
        ?.click();
    });

    expect(container.textContent).toContain("全局策略");
    expect(container.textContent).toContain("图像抓取");
    expect(container.textContent).toContain("光盘封面");
    expect(container.textContent).toContain("壁纸");
    expect(container.textContent).toContain("最小下载宽度");
    expect(container.textContent).toContain("字幕默认值");
    expect(container.textContent).toContain("存储预估");
  });

  it("saves an edited global strategy through the server settings API", async () => {
    const updateSettings = vi.spyOn(api, "updateAdminSettings").mockResolvedValue({
      resumePlayedPercent: 90,
      resumeMinTicks: 1_200_000_000,
      mediaStrategy: {
        metadataLanguage: "zh-CN",
        imageLanguage: "zh-CN",
        region: "CN",
        scraperId: null,
        applyScope: "NEW_CONTENT",
        images: { poster: true, artwork: true, banner: false, logo: true, thumbnail: true, disc: false, wallpaper: false, maxBackdropCount: 1, minDownloadWidth: 1280 },
        subtitles: { autoDownload: false, languages: ["zh-CN"], forcedOnly: false, hearingImpaired: false },
      },
    });
    await renderPage();

    await act(async () => {
      [...container.querySelectorAll<HTMLButtonElement>("[role='tab']")]
        .find((button) => button.textContent?.includes("高级"))
        ?.click();
    });
    const artworkToggle = container.querySelectorAll<HTMLInputElement>(".lux-library-strategy-toggle input")[1];
    await act(async () => artworkToggle?.click());
    await act(async () => {
      [...container.querySelectorAll<HTMLButtonElement>("button")]
        .find((button) => button.textContent?.includes("保存全局策略"))
        ?.click();
    });

    expect(updateSettings).toHaveBeenCalledWith(expect.objectContaining({
      mediaStrategy: expect.objectContaining({
        images: expect.objectContaining({ artwork: true }),
      }),
    }));
  });

  it("lets a library switch from inherited to a custom image strategy", async () => {
    const updateLibrary = vi.spyOn(api, "updateAdminLibrary").mockResolvedValue({ library });
    await renderPage();

    await act(async () => {
      container.querySelector<HTMLButtonElement>("[aria-label='打开 01每日更新 操作菜单']")?.click();
    });
    await act(async () => {
      [...container.querySelectorAll<HTMLButtonElement>("[role='menu'] button")]
        .find((button) => button.textContent?.includes("编辑"))
        ?.click();
    });
    expect(container.textContent).toContain("继承全局");

    const customMode = container.querySelectorAll<HTMLInputElement>(".lux-library-override-modes input")[1];
    await act(async () => customMode?.click());
    await act(async () => {
      [...container.querySelectorAll<HTMLButtonElement>(".lux-library-override-actions button")]
        .find((button) => button.textContent?.includes("保存策略"))
        ?.click();
    });

    expect(updateLibrary).toHaveBeenCalledWith("library-1", expect.objectContaining({
      mediaStrategy: expect.objectContaining({
        images: expect.objectContaining({ poster: true }),
      }),
    }));
  });
});
