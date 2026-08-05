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
  id: "tmdb",
  name: "TMDb 元数据插件",
  description: "使用 TMDb 补全电影和剧集元数据、海报与背景图。",
  category: "SCRAPER",
  version: "1.0.0",
  runtime: "built-in",
  capabilities: ["metadata.search"],
  status: "BUILT_IN_COMPATIBILITY",
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
    description: "可选。留空时使用 Lux 内置的 TMDb Key。",
  }],
  configSource: "BUILT_IN",
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
    vi.spyOn(api, "updateAdminPluginConfig").mockResolvedValue({ plugin: configuredPlugin });
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
