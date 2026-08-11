// @vitest-environment jsdom

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter } from "react-router-dom";
import { AdminPluginsPage, pluginCategoryLabel } from "../src/features/admin/AdminPluginsPage";
import { api } from "../src/lib/api/client";
import type { AdminPlugin } from "../src/lib/api/types";

const pluginLibraryCss = readFileSync(resolve(process.cwd(), "src/features/admin/plugin-library.css"), "utf8");

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const configuredPlugin: AdminPlugin = {
  id: "org.lux.tmdb",
  name: "TMDb 元数据插件",
  description: "使用 TMDb 补全电影和剧集元数据、海报与背景图。",
  category: "SCRAPER",
  version: "1.0.0",
  runtime: "process",
  capabilities: ["metadata.search"],
  status: "READY",
  running: true,
  lastError: null,
  installed: true,
  enabled: true,
  configured: true,
  available: true,
  configurable: true,
  configFields: [{
    key: "apiKey",
    label: "TMDb API Key",
    type: "password",
    required: false,
    sensitive: true,
    description: "可选。留空时使用 TMDb 插件自己的默认凭据。",
  }],
  configSource: "PLUGIN_DEFAULT",
};

let currentPlugin = configuredPlugin;

describe("pluginCategoryLabel", () => {
  it("labels scraper plugins for administrators", () => {
    expect(pluginCategoryLabel("SCRAPER")).toBe("刮削器");
  });

  it("keeps unknown third-party categories visible", () => {
    expect(pluginCategoryLabel("TRANSCODER")).toBe("TRANSCODER");
  });
});

describe("AdminPluginsPage plugin cards", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    currentPlugin = configuredPlugin;
    vi.spyOn(api, "adminPlugins").mockImplementation(async () => ({ plugins: [currentPlugin], total: 1 }));
    vi.spyOn(api, "adminInstalledPlugins").mockImplementation(async () => ({ plugins: currentPlugin.installed ? [currentPlugin] : [], total: currentPlugin.installed ? 1 : 0 }));
    vi.spyOn(api, "adminPluginStore").mockResolvedValue({ url: "https://github.com/Qoo-330ml/Lux-plugins", defaultUrl: "https://github.com/Qoo-330ml/Lux-plugins" });
    vi.spyOn(api, "updateAdminPluginStore").mockResolvedValue({ url: "https://github.com/Qoo-330ml/Lux-plugins", defaultUrl: "https://github.com/Qoo-330ml/Lux-plugins" });
    vi.spyOn(api, "updateAdminPluginConfig").mockResolvedValue({ plugin: configuredPlugin });
    vi.spyOn(api, "updateAdminPluginEnabled").mockImplementation(async (_pluginId, enabled) => ({ plugin: { ...currentPlugin, enabled, available: enabled } }));
    vi.spyOn(api, "runAdminPlugin").mockResolvedValue({ operationId: "operation-1", jobs: [] });
    vi.spyOn(api, "installAdminPlugin").mockResolvedValue({ plugin: configuredPlugin });
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
        createElement(
          QueryClientProvider,
          { client: queryClient },
          createElement(MemoryRouter, null, createElement(AdminPluginsPage)),
        ),
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
  }

  it("keeps plugin cards compact and exposes version, category, and install state", async () => {
    await renderPage();

    const card = container.querySelector<HTMLElement>(".lux-admin-plugin-card");
    expect(card).toBeTruthy();
    expect(card?.textContent).toContain("TMDb 元数据插件");
    expect(card?.textContent).toContain("使用 TMDb 补全电影和剧集元数据、海报与背景图。");
    expect(card?.textContent).toContain("v1.0.0");
    expect(card?.textContent).toContain("刮削器");
    expect(card?.textContent).toContain("已安装");
    expect(card?.textContent).not.toContain("metadata.search");
    expect(card?.textContent).not.toContain("BUILT_IN_COMPATIBILITY");
    expect(card?.querySelector('[aria-label="配置 TMDb 元数据插件"]')).toBeTruthy();
  });

  it("keeps the plugin store source behind a compact entry and opens its settings dialog", async () => {
    await renderPage();

    expect(container.querySelector(".lux-admin-plugin-store")).toBeNull();
    const trigger = container.querySelector<HTMLButtonElement>('[aria-label="设置插件商店来源"]');
    expect(trigger).toBeTruthy();
    expect(trigger?.textContent).toContain("插件商店来源");

    await act(async () => trigger?.click());

    const dialog = container.querySelector<HTMLElement>('[role="dialog"]');
    const input = dialog?.querySelector<HTMLInputElement>("#lux-plugin-store-url");
    expect(dialog).toBeTruthy();
    expect(dialog?.textContent).toContain("插件商店来源");
    expect(input?.value).toBe("https://github.com/Qoo-330ml/Lux-plugins");
    expect(dialog?.querySelector<HTMLButtonElement>('button[type="submit"]')?.disabled).toBe(false);

    await act(async () => {
      dialog?.querySelector<HTMLButtonElement>('[aria-label="关闭插件商店来源设置"]')?.click();
    });
    expect(container.querySelector('[role="dialog"]')).toBeNull();
  });

  it("lays out plugin cards two per row on desktop", async () => {
    vi.mocked(api.adminPlugins).mockResolvedValue({
      plugins: [configuredPlugin, { ...configuredPlugin, id: "org.lux.utility", name: "工具插件" }],
      total: 2,
    });
    await renderPage();

    const grid = container.querySelector<HTMLElement>(".lux-admin-plugin-grid");
    expect(grid).toBeTruthy();
    expect(grid?.querySelectorAll(":scope > .lux-admin-plugin-card")).toHaveLength(2);
    expect(pluginLibraryCss).toMatch(/\.lux-admin-plugin-grid\s*\{[^}]*grid-template-columns:\s*repeat\(2,\s*minmax\(0,\s*1fr\)\);/s);
  });

  it("opens configuration in a separate dialog card", async () => {
    await renderPage();

    await act(async () => {
      container.querySelector<HTMLButtonElement>('[aria-label="配置 TMDb 元数据插件"]')?.click();
    });

    const dialog = container.querySelector<HTMLElement>('[role="dialog"]');
    expect(dialog).toBeTruthy();
    expect(dialog?.getAttribute("aria-modal")).toBe("true");
    expect(dialog?.textContent).toContain("TMDb API Key");
    expect(dialog?.querySelector('input[type="password"]')).toBeTruthy();

    await act(async () => {
      dialog?.querySelector<HTMLButtonElement>('[aria-label="关闭 TMDb 元数据插件配置"]')?.click();
    });
    expect(container.querySelector('[role="dialog"]')).toBeNull();
  });

  it("renders TMDb language preference, fallback switch, and ordered multi-select", async () => {
    currentPlugin = {
      ...configuredPlugin,
      configValues: {
        preferredLanguage: "zh-CN",
        languageFallbackEnabled: false,
        fallbackLanguages: ["zh-SG", "zh-HK", "zh-TW"],
        alternateApiEnabled: false,
        apiBaseUrl: "https://api.themoviedb.org",
      },
      configFields: [
        ...configuredPlugin.configFields,
        {
          key: "preferredLanguage",
          label: "首选语言",
          type: "select",
          required: true,
          sensitive: false,
          options: [
            { value: "zh-CN", label: "简体中文" },
            { value: "zh-SG", label: "zh-SG" },
            { value: "zh-HK", label: "zh-HK" },
          ],
        },
        {
          key: "languageFallbackEnabled",
          label: "TMDb 语言回退",
          type: "toggle",
          required: false,
          sensitive: false,
          description: "按顺序补全缺失元数据。",
        },
        {
          key: "fallbackLanguages",
          label: "备选语言顺序",
          type: "select",
          required: false,
          sensitive: false,
          multiple: true,
          options: [
            { value: "zh-SG", label: "zh-SG" },
            { value: "zh-HK", label: "zh-HK" },
            { value: "zh-TW", label: "zh-TW" },
          ],
        },
        {
          key: "alternateApiEnabled",
          label: "替代 API 地址",
          type: "toggle",
          required: false,
          sensitive: false,
          description: "开启后使用下方地址访问 TMDb。",
        },
        {
          key: "apiBaseUrl",
          label: "TMDb API 地址",
          type: "select",
          required: true,
          sensitive: false,
          options: [
            { value: "official", label: "https://api.themoviedb.org" },
            { value: "alternate", label: "https://api.tmdb.org" },
            { value: "custom", label: "自定义" },
          ],
        },
      ],
    };
    await renderPage();

    await act(async () => {
      container.querySelector<HTMLButtonElement>('[aria-label="配置 TMDb 元数据插件"]')?.click();
    });

    const dialog = container.querySelector<HTMLElement>('[role="dialog"]');
    const selects = Array.from(dialog?.querySelectorAll("select") ?? []);
    expect(selects[0]?.value).toBe("zh-CN");
    expect(selects[1]?.multiple).toBe(true);
    expect(Array.from(selects[1]?.selectedOptions ?? [], (option) => option.value)).toEqual(["zh-SG", "zh-HK", "zh-TW"]);
    expect(selects[2]?.value).toBe("official");
    expect(selects[2]?.options[1]?.textContent).toBe("https://api.tmdb.org");
    expect(dialog?.querySelectorAll('input[type="checkbox"]')).toHaveLength(2);

    await act(async () => {
      dialog?.querySelector<HTMLButtonElement>('button[type="submit"]')?.click();
    });
    expect(api.updateAdminPluginConfig).toHaveBeenCalledWith("org.lux.tmdb", expect.objectContaining({
      preferredLanguage: "zh-CN",
      languageFallbackEnabled: false,
      fallbackLanguages: ["zh-SG", "zh-HK", "zh-TW"],
      alternateApiEnabled: false,
      apiBaseUrl: "https://api.themoviedb.org",
    }));
  });

  it("keeps the install action in the top-right corner for store items", async () => {
    currentPlugin = {
      ...configuredPlugin,
      installed: false,
      enabled: false,
      available: false,
    };
    await renderPage();

    expect(container.querySelector('[aria-label="安装 TMDb 元数据插件"]')).toBeTruthy();
    expect(container.querySelector('[aria-label="插件状态：已安装"]')).toBeNull();
  });

  it("replaces the installed badge with an accessible enable switch in installed management", async () => {
    await renderPage();

    await act(async () => {
      container.querySelector<HTMLButtonElement>('[aria-pressed="false"]')?.click();
    });

    const card = container.querySelector<HTMLElement>(".lux-admin-plugin-card");
    const toggle = card?.querySelector<HTMLButtonElement>('[role="switch"]');
    expect(toggle).toBeTruthy();
    expect(toggle?.getAttribute("aria-checked")).toBe("true");
    expect(toggle?.getAttribute("aria-label")).toBe("禁用 TMDb 元数据插件");
    expect(toggle?.textContent).toContain("已启用");
    expect(card?.textContent).not.toContain("已安装");
    expect(card?.querySelector('[aria-label="插件状态：已安装"]')).toBeNull();
    expect(pluginLibraryCss).toMatch(/\.lux-admin-plugin-enable-switch\s*\{/);

    await act(async () => {
      toggle?.click();
    });
    expect(api.updateAdminPluginEnabled).toHaveBeenCalledWith("org.lux.tmdb", false);
  });

  it("shows a disabled installed plugin as off and offers to enable it", async () => {
    currentPlugin = {
      ...configuredPlugin,
      enabled: false,
      available: false,
      status: "DISABLED",
      unavailableReason: "DISABLED",
    };
    await renderPage();

    await act(async () => {
      container.querySelector<HTMLButtonElement>('[aria-pressed="false"]')?.click();
    });

    const toggle = container.querySelector<HTMLButtonElement>('[role="switch"]');
    expect(toggle?.getAttribute("aria-checked")).toBe("false");
    expect(toggle?.getAttribute("aria-label")).toBe("启用 TMDb 元数据插件");
    expect(toggle?.textContent).toContain("已禁用");
  });

  it("renders media-info settings and runs with the saved plugin configuration", async () => {
    currentPlugin = {
      ...configuredPlugin,
      id: "org.lux.strm-media-info",
      name: "strm媒体信息提取",
      category: "MEDIA",
      configSource: "PLUGIN_CONFIG",
      configValues: {
        libraryIds: ["library-1"],
        concurrency: 2,
        existingInfoPolicy: "SKIP",
        mediaInfoEnabled: true,
        thumbnailEnabled: true,
        thumbnailPositionPercent: 30,
        writeSidecars: true,
        schedule: "0 3 * * *",
      },
      configFields: [
        { key: "libraryIds", label: "媒体库", type: "select", required: true, sensitive: false, multiple: true, optionsSource: "media-libraries", options: [{ value: "library-1", label: "电影库" }, { value: "library-2", label: "剧集库" }] },
        { key: "concurrency", label: "并发数", type: "number", required: true, sensitive: false, defaultValue: 2, minimum: 1, maximum: 64 },
        { key: "existingInfoPolicy", label: "已有媒体信息处理方式", type: "select", required: false, sensitive: false, defaultValue: "SKIP", options: [{ value: "SKIP", label: "跳过已有媒体信息" }, { value: "OVERWRITE", label: "覆盖已有媒体信息" }] },
        { key: "mediaInfoEnabled", label: "提取媒体信息", type: "toggle", required: false, sensitive: false, defaultValue: true, description: "使用 ffprobe 提取媒体轨道信息。" },
        { key: "thumbnailEnabled", label: "补全 STRM 缩略图", type: "toggle", required: false, sensitive: false, defaultValue: false, description: "仅为缺失或无效的 STRM 缩略图使用 ffmpeg 截图。" },
        { key: "thumbnailPositionPercent", label: "缩略图位置", type: "number", required: false, sensitive: false, defaultValue: 30, minimum: 1, maximum: 99, description: "按视频时长百分比截图。" },
        { key: "writeSidecars", label: "写入 mediainfo.json", type: "toggle", required: false, sensitive: false },
        { key: "schedule", label: "执行计划", type: "text", required: true, sensitive: false, defaultValue: "0 3 * * *" },
      ],
    };
    await renderPage();

    expect(container.querySelector('[aria-label="开始提取"]')).toBeTruthy();
    await act(async () => {
      container.querySelector<HTMLButtonElement>('[aria-label="配置 strm媒体信息提取"]')?.click();
    });
    const dialog = container.querySelector<HTMLElement>('[role="dialog"]');
    const selects = Array.from(dialog?.querySelectorAll("select") ?? []);
    expect(selects[0]?.multiple).toBe(true);
    expect(selects[0]?.options[0]?.textContent).toBe("电影库");
    expect(dialog?.querySelector('input[type="number"]')).toBeTruthy();
    expect(dialog?.querySelectorAll('select')).toHaveLength(2);
    expect(dialog?.querySelectorAll('input[type="checkbox"]')).toHaveLength(3);
    expect(dialog?.textContent).toContain("提取媒体信息");
    expect(dialog?.textContent).toContain("补全 STRM 缩略图");
    expect(dialog?.querySelector<HTMLInputElement>('[id$="-thumbnail-position-percent"]')?.value).toBe("30");
    expect(pluginLibraryCss).toMatch(/\.lux-admin-plugin-dialog\s*\{[^}]*max-height:\s*calc\(100vh - 48px\);/s);

    await act(async () => {
      dialog?.querySelector<HTMLButtonElement>('button[type="submit"]')?.click();
    });
    expect(api.updateAdminPluginConfig).toHaveBeenCalledWith("org.lux.strm-media-info", expect.objectContaining({
      libraryIds: ["library-1"],
      concurrency: 2,
      existingInfoPolicy: "SKIP",
      mediaInfoEnabled: true,
      thumbnailEnabled: true,
      thumbnailPositionPercent: 30,
      writeSidecars: true,
      schedule: "0 3 * * *",
    }));
    await act(async () => {
      container.querySelector<HTMLButtonElement>('[aria-label="开始提取"]')?.click();
    });
    expect(api.runAdminPlugin).toHaveBeenCalledWith("org.lux.strm-media-info");
  });

  it("does not show configuration action for plugins without configuration", async () => {
    currentPlugin = {
      ...configuredPlugin,
      id: "org.lux.utility",
      name: "工具插件",
      configurable: false,
      configFields: [],
    };
    await renderPage();

    expect(container.querySelector('[aria-label="配置 工具插件"]')).toBeNull();
  });
});
