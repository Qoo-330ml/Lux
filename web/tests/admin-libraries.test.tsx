// @vitest-environment jsdom

import { readFileSync } from "node:fs";
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
  realtimeMetadataAutoMatchEnabled: false,
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
  configSource: "PLUGIN_DEFAULT",
};

const backupScraper: AdminPlugin = {
  ...configuredScraper,
  id: "org.lux.backup",
  name: "备用元数据插件",
};

const mediaProbePlugin: AdminPlugin = {
  id: "org.lux.strm-media-info",
  name: "strm媒体信息提取",
  description: "使用 ffprobe 提取 STRM 外部媒体的技术信息。",
  category: "MEDIA",
  version: "1.0.0",
  runtime: "process",
  capabilities: ["media.probe"],
  status: "READY",
  running: false,
  lastError: null,
  installed: true,
  enabled: true,
  configured: true,
  available: true,
  unavailableReason: null,
  configurable: false,
  configFields: [],
  configSource: "NONE",
};

describe("AdminLibrariesPage library cards", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [library] });
    vi.spyOn(api, "adminPlugins").mockResolvedValue({ plugins: [configuredScraper] });
    vi.spyOn(api, "adminChapterSources").mockResolvedValue({ sources: [] });
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
          writeToMetadata: false,
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
    vi.unstubAllGlobals();
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

  it("creates a library with multiple folders selected from the same dialog", async () => {
    const createdLibrary = { ...library, id: "library-2", name: "电影库", roots: [] };
    const createLibrary = vi.spyOn(api, "createAdminLibrary").mockResolvedValue({ library: createdLibrary });
    const addRoot = vi.spyOn(api, "addAdminLibraryRoot").mockResolvedValue({
      root: {
        ...library.roots[0],
        id: "root-2",
        libraryId: "library-2",
        canonicalPath: "/media",
        displayPath: "/media",
      },
    });
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({
      path: "/",
      parentPath: null,
      directories: [{ name: "media", path: "/media" }],
      page: 1,
      pageSize: 50,
      hasMore: false,
    }), { status: 200, headers: { "content-type": "application/json" } }));
    vi.stubGlobal("fetch", fetchMock);
    await renderPage();

    await act(async () => {
      [...container.querySelectorAll<HTMLButtonElement>("button")]
        .find((button) => button.textContent?.includes("新增媒体库"))
        ?.click();
    });

    const dialog = container.querySelector('[role="dialog"]');
    expect(dialog?.textContent).toContain("文件夹");
    expect(
      dialog?.querySelector<HTMLInputElement>("[aria-label='新媒体库实时新增资源自动刮削']")?.checked,
    ).toBe(true);
    const nameInput = dialog?.querySelector<HTMLInputElement>("#new-library-name");
    const rootInput = dialog?.querySelector<HTMLInputElement>("[aria-label='新媒体库根路径']");
    expect(nameInput).toBeTruthy();
    expect(rootInput).toBeTruthy();

    await act(async () => {
      if (!nameInput) throw new Error("new library name input missing");
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(nameInput, "电影库");
      nameInput.dispatchEvent(new Event("input", { bubbles: true }));
      nameInput.dispatchEvent(new Event("change", { bubbles: true }));
      dialog?.querySelector<HTMLButtonElement>("[aria-label='浏览服务器目录']")?.click();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    await act(async () => {
      dialog?.querySelector<HTMLButtonElement>("[aria-label='选择目录 /media']")?.click();
    });
    const usePathButton = [...container.querySelectorAll<HTMLButtonElement>("button")]
      .find((button) => button.textContent?.includes("使用此路径"));
    await act(async () => usePathButton?.click());

    expect(rootInput?.value).toBe("");
    expect(dialog?.textContent).toContain("/media");
    await act(async () => {
      if (!rootInput) throw new Error("new library root input missing");
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(rootInput, "/shows");
      rootInput.dispatchEvent(new Event("input", { bubbles: true }));
      rootInput.dispatchEvent(new Event("change", { bubbles: true }));
      dialog?.querySelector<HTMLButtonElement>("[aria-label='添加新媒体库路径']")?.click();
    });
    expect(rootInput?.value).toBe("");
    expect(dialog?.textContent).toContain("/shows");
    await act(async () => {
      [...(dialog?.querySelectorAll<HTMLButtonElement>("button") ?? [])]
        .find((button) => button.textContent?.includes("创建媒体库"))
        ?.click();
      await vi.waitFor(() => expect(addRoot).toHaveBeenCalledTimes(2));
    });

    expect(createLibrary).toHaveBeenCalledWith({
      name: "电影库",
      kind: "MOVIE",
      scraperId: null,
      realtimeMetadataAutoMatchEnabled: true,
    });
    expect(createLibrary.mock.invocationCallOrder[0]).toBeLessThan(addRoot.mock.invocationCallOrder[0]);
    expect(addRoot).toHaveBeenNthCalledWith(1, "library-2", "/media");
    expect(addRoot).toHaveBeenNthCalledWith(2, "library-2", "/shows");
    expect(container.querySelector("#new-library-title")).toBeNull();
  });

  it("keeps the create dialog form scrollable when the directory picker is open", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(JSON.stringify({
      path: "/",
      parentPath: null,
      directories: [{ name: "media", path: "/media" }],
      page: 1,
      pageSize: 50,
      hasMore: false,
    }), { status: 200, headers: { "content-type": "application/json" } })));
    await renderPage();

    await act(async () => {
      [...container.querySelectorAll<HTMLButtonElement>("button")]
        .find((button) => button.textContent?.includes("新增媒体库"))
        ?.click();
    });
    const dialog = container.querySelector('[role="dialog"]');
    await act(async () => {
      dialog?.querySelector<HTMLButtonElement>("[aria-label='浏览服务器目录']")?.click();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const form = container.querySelector<HTMLFormElement>(".lux-library-create-dialog form");
    expect(form).toBeTruthy();
    const stylesheet = readFileSync(`${process.cwd()}/src/react.css`, "utf8");
    expect(stylesheet).toContain(".lux-library-create-dialog > .lux-library-dialog-form { min-height: 0; overflow-y: auto; }");
  });

  it("does not leave a retryable create form when folder addition is not completed", async () => {
    const createdLibrary = { ...library, id: "library-2", name: "电影库", roots: [] };
    const createLibrary = vi.spyOn(api, "createAdminLibrary").mockResolvedValue({ library: createdLibrary });
    const addRoot = vi.spyOn(api, "addAdminLibraryRoot").mockRejectedValue(new Error("路径不可用"));
    await renderPage();

    await act(async () => {
      [...container.querySelectorAll<HTMLButtonElement>("button")]
        .find((button) => button.textContent?.includes("新增媒体库"))
        ?.click();
    });
    const dialog = container.querySelector('[role="dialog"]');
    const nameInput = dialog?.querySelector<HTMLInputElement>("#new-library-name");
    const rootInput = dialog?.querySelector<HTMLInputElement>("[aria-label='新媒体库根路径']");
    await act(async () => {
      if (!nameInput || !rootInput) throw new Error("new library inputs missing");
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(nameInput, "电影库");
      nameInput.dispatchEvent(new Event("input", { bubbles: true }));
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(rootInput, "/media");
      rootInput.dispatchEvent(new Event("input", { bubbles: true }));
      dialog?.querySelector<HTMLButtonElement>("button[type='submit']")?.click();
      await vi.waitFor(() => expect(addRoot).toHaveBeenCalledWith("library-2", "/media"));
    });

    expect(createLibrary).toHaveBeenCalledOnce();
    expect(container.querySelector("#new-library-title")).toBeNull();
    expect(container.textContent).toContain("媒体库已创建");
    expect(container.textContent).toContain("路径不可用");
  });

  it("renders a library card with its cover, type, and root path", async () => {
    await renderPage();

    expect(container.querySelector(".lux-admin-library-grid")).toBeTruthy();
    expect(container.querySelector(".lux-admin-library-cover")?.getAttribute("src")).toBe("/covers/daily-updates.jpg");
    expect(container.textContent).toContain("01每日更新");
    expect(container.textContent).toContain("混合内容");
    expect(container.textContent).toContain("/media/strm/video/每日更新");
  });

  it("summarizes multiple library roots on the card", async () => {
    vi.mocked(api.adminLibraries).mockResolvedValueOnce({
      libraries: [{
        ...library,
        roots: [
          ...library.roots,
          { ...library.roots[0], id: "root-2", displayPath: "/media/strm/video/电影" },
          { ...library.roots[0], id: "root-3", displayPath: "/media/strm/video/剧集" },
        ],
      }],
    });

    await renderPage();

    expect(container.textContent).toContain("3个文件夹");
    expect(container.textContent).not.toContain("/media/strm/video/每日更新");
  });

  it("opens the library actions menu from the card overflow button", async () => {
    await renderPage();

    const menuButton = container.querySelector<HTMLButtonElement>("[aria-label='打开 01每日更新 操作菜单']");
    expect(menuButton).toBeTruthy();

    await act(async () => menuButton?.click());

    expect(container.querySelector('[role="menu"]')?.textContent).toContain("编辑");
    expect(container.querySelector('[role="menu"]')?.textContent).toContain("扫描媒体库文件");
  });

  it("shows why a library could not be deleted", async () => {
    const deleteLibrary = vi.spyOn(api, "deleteAdminLibrary").mockRejectedValue(new Error("媒体库仍有扫描任务运行"));
    vi.spyOn(window, "confirm").mockReturnValue(true);
    await renderPage();

    await act(async () => {
      container.querySelector<HTMLButtonElement>("[aria-label='打开 01每日更新 操作菜单']")?.click();
    });
    const removeAction = [...container.querySelectorAll<HTMLButtonElement>("[role='menu'] button")]
      .find((button) => button.textContent?.includes("移除"));
    expect(removeAction).toBeTruthy();

    await act(async () => {
      removeAction?.click();
      await vi.waitFor(() => expect(deleteLibrary).toHaveBeenCalledWith("library-1"));
    });

    expect(container.querySelector('[role="alert"]')?.textContent).toContain("媒体库仍有扫描任务运行");
    expect(container.textContent).toContain("01每日更新");
  });

  it("removes a library card after the delete request succeeds", async () => {
    let listedLibraries = [library];
    vi.mocked(api.adminLibraries).mockImplementation(async () => ({ libraries: listedLibraries }));
    const deleteLibrary = vi.spyOn(api, "deleteAdminLibrary").mockImplementation(async (libraryId) => {
      listedLibraries = listedLibraries.filter((item) => item.id !== libraryId);
    });
    vi.spyOn(window, "confirm").mockReturnValue(true);
    await renderPage();

    await act(async () => {
      container.querySelector<HTMLButtonElement>("[aria-label='打开 01每日更新 操作菜单']")?.click();
    });
    const removeAction = [...container.querySelectorAll<HTMLButtonElement>("[role='menu'] button")]
      .find((button) => button.textContent?.includes("移除"));
    await act(async () => {
      removeAction?.click();
      await vi.waitFor(() => expect(deleteLibrary).toHaveBeenCalledWith("library-1"));
    });

    expect(container.querySelector(".lux-admin-library-card")).toBeNull();
    expect(container.textContent).toContain("还没有媒体库");
  });

  it("starts metadata refresh from the library actions menu", async () => {
    const refresh = vi.spyOn(api, "startLibraryMetadataRefresh").mockResolvedValue({
      totalCount: 1,
      mode: "FILL_MISSING",
      job: { id: "job-1", status: "QUEUED", mode: "FILL_MISSING", totalCount: 1, processedCount: 0, createdAt: 0 },
    });
    await renderPage();

    await act(async () => {
      container.querySelector<HTMLButtonElement>("[aria-label='打开 01每日更新 操作菜单']")?.click();
    });
    const refreshAction = [...container.querySelectorAll<HTMLButtonElement>("[role='menu'] button")]
      .find((button) => button.textContent?.includes("刷新元数据"));
    expect(refreshAction).toBeTruthy();
    expect(refreshAction?.disabled).toBe(false);

    await act(async () => {
      refreshAction?.click();
      await Promise.resolve();
    });

    expect(refresh).toHaveBeenCalledWith("library-1", "FILL_MISSING");
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
    expect(container.querySelector('[role="dialog"]')?.textContent).not.toContain("增量扫描");
    expect(container.querySelector('[role="dialog"]')?.textContent).not.toContain("全量校验");
    expect(container.querySelector('[role="dialog"]')?.textContent).not.toContain("元数据任务");
  });

  it("allows a mixed library to choose an intro and outro source", async () => {
    vi.mocked(api.adminChapterSources).mockResolvedValue({
      sources: [{
        id: "org.lux.intro-outro-detector",
        name: "片头片尾检测",
        description: "检测片头片尾",
        version: "1.0.0",
        capabilities: ["chapters.detect"],
        lookup: false,
      }],
    });
    await renderPage();

    await act(async () => {
      container.querySelector<HTMLButtonElement>("[aria-label='打开 01每日更新 操作菜单']")?.click();
    });
    await act(async () => {
      [...container.querySelectorAll<HTMLButtonElement>("[role='menu'] button")]
        .find((button) => button.textContent?.includes("编辑"))
        ?.click();
    });

    const sourceSelect = container.querySelector<HTMLButtonElement>("[aria-label='片头片尾数据源']");
    expect(sourceSelect?.disabled).toBe(false);
    expect(container.textContent).toContain("选择后，该媒体库的检测任务和章节输出只使用此来源。");
  });

  it("keeps an unavailable configured source clearable", async () => {
    vi.mocked(api.adminLibraries).mockResolvedValue({
      libraries: [{ ...library, chapterSourceId: "org.lux.missing-chapter-source" }],
    });
    await renderPage();

    await act(async () => {
      container.querySelector<HTMLButtonElement>("[aria-label='打开 01每日更新 操作菜单']")?.click();
    });
    await act(async () => {
      [...container.querySelectorAll<HTMLButtonElement>("[role='menu'] button")]
        .find((button) => button.textContent?.includes("编辑"))
        ?.click();
    });

    expect(container.querySelector<HTMLButtonElement>("[aria-label='片头片尾数据源']")?.textContent)
      .toContain("已配置来源（暂不可用）");
    const clearButton = container.querySelector<HTMLButtonElement>("[aria-label='清除片头片尾数据源配置']");
    expect(clearButton).toBeTruthy();
    const updateLibrary = vi.spyOn(api, "updateAdminLibrary").mockResolvedValue({ library });
    await act(async () => {
      clearButton?.click();
      await vi.waitFor(() => expect(updateLibrary).toHaveBeenCalledWith("library-1", { chapterSourceId: null }));
    });
  });

  it("updates the realtime metadata auto-match switch", async () => {
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
    const toggle = container.querySelector<HTMLInputElement>("[aria-label='01每日更新 实时新增资源自动刮削']");
    expect(toggle?.checked).toBe(false);

    await act(async () => {
      toggle?.click();
      await vi.waitFor(() => expect(updateLibrary).toHaveBeenCalledWith("library-1", { realtimeMetadataAutoMatchEnabled: true }));
    });
  });

  it("adds a selected server directory as a library root", async () => {
    const addRoot = vi.spyOn(api, "addAdminLibraryRoot").mockResolvedValue({
      root: {
        ...library.roots[0],
        id: "root-2",
        canonicalPath: "/media",
        displayPath: "/media",
      },
    });
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({
      path: "/",
      parentPath: null,
      directories: [{ name: "media", path: "/media" }],
      page: 1,
      pageSize: 50,
      hasMore: false,
    }), { status: 200, headers: { "content-type": "application/json" } }));
    vi.stubGlobal("fetch", fetchMock);
    await renderPage();

    await act(async () => {
      container.querySelector<HTMLButtonElement>("[aria-label='打开 01每日更新 操作菜单']")?.click();
    });
    await act(async () => {
      [...container.querySelectorAll<HTMLButtonElement>("[role='menu'] button")]
        .find((button) => button.textContent?.includes("编辑"))
        ?.click();
    });

    const browseButton = container.querySelector<HTMLButtonElement>('[aria-label="浏览服务器目录"]');
    expect(browseButton).toBeTruthy();
    expect(browseButton?.textContent).toBe("");
    expect(browseButton?.querySelector("svg")).toBeTruthy();
    await act(async () => {
      browseButton?.click();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/v1/admin/directories?path=%2F&page=1&pageSize=50",
      expect.objectContaining({ credentials: "same-origin" }),
    );
    const mediaDirectory = container.querySelector<HTMLButtonElement>('[aria-label="选择目录 /media"]');
    expect(mediaDirectory).toBeTruthy();
    await act(async () => mediaDirectory?.click());
    await act(async () => {
      [...container.querySelectorAll<HTMLButtonElement>("button")]
        .find((button) => button.textContent?.includes("使用此路径"))
        ?.click();
      await vi.waitFor(() => expect(addRoot).toHaveBeenCalledWith("library-1", "/media"));
    });

    expect(container.querySelector<HTMLInputElement>('[aria-label="01每日更新 新根路径"]')?.value).toBe("");
    expect(container.querySelector("#lux-directory-picker-title")).toBeNull();
    expect([...container.querySelectorAll<HTMLButtonElement>("button")]
      .some((button) => button.textContent?.includes("添加路径"))).toBe(false);
  });

  it("adds a manually entered path from the directory picker", async () => {
    const addRoot = vi.spyOn(api, "addAdminLibraryRoot").mockResolvedValue({
      root: {
        ...library.roots[0],
        id: "root-3",
        canonicalPath: "/manual/media",
        displayPath: "/manual/media",
      },
    });
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(JSON.stringify({
      path: "/",
      parentPath: null,
      directories: [],
      page: 1,
      pageSize: 50,
      hasMore: false,
    }), { status: 200, headers: { "content-type": "application/json" } })));
    await renderPage();

    await act(async () => {
      container.querySelector<HTMLButtonElement>("[aria-label='打开 01每日更新 操作菜单']")?.click();
    });
    await act(async () => {
      [...container.querySelectorAll<HTMLButtonElement>("[role='menu'] button")]
        .find((button) => button.textContent?.includes("编辑"))
        ?.click();
    });

    const pathInput = container.querySelector<HTMLInputElement>('[aria-label="01每日更新 新根路径"]');
    expect(pathInput).toBeTruthy();
    await act(async () => {
      if (!pathInput) throw new Error("library root path input missing");
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(pathInput, "/manual/media");
      pathInput.dispatchEvent(new Event("input", { bubbles: true }));
      pathInput.dispatchEvent(new Event("change", { bubbles: true }));
    });
    await act(async () => {
      container.querySelector<HTMLButtonElement>('[aria-label="浏览服务器目录"]')?.click();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(container.querySelector(".lux-directory-picker-selected")?.textContent).toContain("/manual/media");

    await act(async () => {
      [...container.querySelectorAll<HTMLButtonElement>("button")]
        .find((button) => button.textContent?.includes("使用此路径"))
        ?.click();
      await vi.waitFor(() => expect(addRoot).toHaveBeenCalledWith("library-1", "/manual/media"));
    });

    expect(pathInput?.value).toBe("");
    expect(container.querySelector("#lux-directory-picker-title")).toBeNull();
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

  it("does not list media probe plugins as library scrapers", async () => {
    vi.mocked(api.adminPlugins).mockResolvedValue({ plugins: [configuredScraper, mediaProbePlugin] });
    await renderPage();

    await act(async () => {
      container.querySelector<HTMLButtonElement>("[aria-label='打开 01每日更新 操作菜单']")?.click();
    });
    await act(async () => {
      [...container.querySelectorAll<HTMLButtonElement>("[role='menu'] button")]
        .find((button) => button.textContent?.includes("编辑"))
        ?.click();
    });
    const scraperTrigger = container.querySelector<HTMLButtonElement>("[aria-label='刮削器']");
    expect(scraperTrigger).toBeTruthy();

    await act(async () => scraperTrigger?.click());

    expect(document.body.textContent).not.toContain("strm媒体信息提取");
  });

  it("edits ordered scraper roles and keeps the primary role fixed", async () => {
    vi.mocked(api.adminLibraries).mockResolvedValue({
      libraries: [{
        ...library,
        scrapers: [
          { scraperId: configuredScraper.id, position: 0, role: "PRIMARY" },
          { scraperId: backupScraper.id, position: 1, role: "BACKUP" },
        ],
      }],
    });
    vi.mocked(api.adminPlugins).mockResolvedValue({ plugins: [configuredScraper, backupScraper] });
    const update = vi.spyOn(api, "updateAdminLibrary").mockResolvedValue({ library });
    await renderPage();

    await act(async () => container.querySelector<HTMLButtonElement>("[aria-label='打开 01每日更新 操作菜单']")?.click());
    await act(async () => [...container.querySelectorAll<HTMLButtonElement>("[role='menu'] button")]
      .find((button) => button.textContent?.includes("编辑"))?.click());

    expect(container.textContent).toContain("TMDb 元数据插件");
    expect(container.textContent).toContain("备用元数据插件");
    expect(container.querySelectorAll(".lux-admin-scraper-actions")).toHaveLength(2);
    const role = container.querySelector<HTMLButtonElement>("[aria-label='刮削器 备用元数据插件 角色']");
    expect(role).toBeTruthy();
    await act(async () => role?.click());
    await act(async () => document.querySelector<HTMLButtonElement>("[data-value='SUPPLEMENT']")?.click());
    expect(update).toHaveBeenCalledWith("library-1", {
      scrapers: [
        { scraperId: configuredScraper.id, role: "PRIMARY" },
        { scraperId: backupScraper.id, role: "SUPPLEMENT" },
      ],
    });

    const moveDown = container.querySelector<HTMLButtonElement>("[aria-label='下移刮削器 TMDb 元数据插件']");
    expect(moveDown).toBeTruthy();
    await act(async () => moveDown?.click());
    expect(update).toHaveBeenLastCalledWith("library-1", {
      scrapers: [
        { scraperId: backupScraper.id, role: "PRIMARY" },
        { scraperId: configuredScraper.id, role: "BACKUP" },
      ],
    });
  });

  it("shows a selected scraper that is no longer available", async () => {
    vi.mocked(api.adminLibraries).mockResolvedValue({
      libraries: [{
        ...library,
        scrapers: [{ scraperId: "org.lux.removed", position: 0, role: "PRIMARY" }],
      }],
    });
    await renderPage();

    await act(async () => container.querySelector<HTMLButtonElement>("[aria-label='打开 01每日更新 操作菜单']")?.click());
    await act(async () => [...container.querySelectorAll<HTMLButtonElement>("[role='menu'] button")]
      .find((button) => button.textContent?.includes("编辑"))?.click());

    expect(container.textContent).toContain("org.lux.removed（暂不可用）");
  });

  it("keeps scraper row actions inline and the clear action readable", () => {
    const stylesheet = readFileSync(`${process.cwd()}/src/react.css`, "utf8");

    expect(stylesheet).toMatch(/\.lux-admin-scraper-actions\s*\{[^}]*display:\s*flex;[^}]*align-items:\s*center;/s);
    expect(stylesheet).toMatch(/\.lux-admin-scraper-field\s+\.lux-admin-scraper-clear\s*\{[^}]*width:\s*auto;[^}]*min-height:\s*32px;[^}]*white-space:\s*nowrap;/s);
  });

  it("shows global image and subtitle defaults in the strategy view", async () => {
    await renderPage();

    await act(async () => {
      [...container.querySelectorAll<HTMLButtonElement>("[role='tab']")]
        .find((button) => button.textContent?.includes("高级"))
        ?.click();
    });

    expect(container.textContent).toContain("全局策略");
    expect(container.textContent).toContain("额外保存元数据到 metadata");
    expect(container.textContent).toContain("图像抓取");
    expect(container.textContent).toContain("光盘封面");
    expect(container.textContent).toContain("壁纸");
    expect(container.textContent).toContain("最小下载宽度");
    expect(container.textContent).toContain("字幕默认值");
    expect(container.textContent).toContain("存储预估");
  });

  it("lets the global strategy choose a metadata refresh mode", async () => {
    await renderPage();

    await act(async () => {
      [...container.querySelectorAll<HTMLButtonElement>("[role='tab']")]
        .find((button) => button.textContent?.includes("高级"))
        ?.click();
    });

    expect(container.textContent).toContain("元数据刮削模式");
    const modeTrigger = container.querySelector<HTMLButtonElement>("[aria-label='元数据刮削模式']");
    expect(modeTrigger).toBeTruthy();
    await act(async () => modeTrigger?.click());
    const fullRefresh = document.querySelector<HTMLButtonElement>("[data-value='FULL_REFRESH']");
    expect(fullRefresh).toBeTruthy();
    await act(async () => fullRefresh?.click());
    expect(modeTrigger?.textContent).toContain("完整刮削");
    expect(container.textContent).toContain("锁定的 NFO 字段不会被替换");
  });

  it("starts a global refresh using the selected mode", async () => {
    const refresh = vi.spyOn(api, "startLibraryMetadataRefresh").mockResolvedValue({
      totalCount: 1,
      mode: "FULL_REFRESH",
      job: { id: "job-1", status: "QUEUED", mode: "FULL_REFRESH", totalCount: 1, processedCount: 0, createdAt: 0 },
    });
    await renderPage();

    await act(async () => {
      [...container.querySelectorAll<HTMLButtonElement>("[role='tab']")]
        .find((button) => button.textContent?.includes("高级"))
        ?.click();
    });
    await act(async () => container.querySelector<HTMLButtonElement>("[aria-label='元数据刮削模式']")?.click());
    await act(async () => document.querySelector<HTMLButtonElement>("[data-value='FULL_REFRESH']")?.click());
    await act(async () => {
      [...container.querySelectorAll<HTMLButtonElement>("button")]
        .find((button) => button.textContent?.includes("开始全局刮削"))
        ?.click();
      await Promise.resolve();
    });

    expect(refresh).toHaveBeenCalledWith("library-1", "FULL_REFRESH");
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
        images: { poster: true, artwork: true, banner: false, logo: true, thumbnail: true, disc: false, wallpaper: false, writeToMetadata: false, maxBackdropCount: 1, minDownloadWidth: 1280 },
        subtitles: { autoDownload: false, languages: ["zh-CN"], forcedOnly: false, hearingImpaired: false },
      },
    });
    await renderPage();

    await act(async () => {
      [...container.querySelectorAll<HTMLButtonElement>("[role='tab']")]
        .find((button) => button.textContent?.includes("高级"))
        ?.click();
    });
    const artworkToggle = [...container.querySelectorAll<HTMLLabelElement>(".lux-library-strategy-toggle")]
      .find((label) => label.textContent?.includes("艺术图"))
      ?.querySelector<HTMLInputElement>("input");
    const metadataToggle = [...container.querySelectorAll<HTMLLabelElement>(".lux-library-strategy-toggle")]
      .find((label) => label.textContent?.includes("额外保存元数据到 metadata"))
      ?.querySelector<HTMLInputElement>("input");
    await act(async () => artworkToggle?.click());
    await act(async () => metadataToggle?.click());
    await act(async () => {
      [...container.querySelectorAll<HTMLButtonElement>("button")]
        .find((button) => button.textContent?.includes("保存全局策略"))
        ?.click();
    });

    expect(updateSettings).toHaveBeenCalledWith(expect.objectContaining({
      mediaStrategy: expect.objectContaining({
        images: expect.objectContaining({ artwork: true, writeToMetadata: true }),
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
