// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { EmbyMigrationPluginConfig } from "../src/features/admin/EmbyMigrationPluginConfig";
import { api } from "../src/lib/api/client";
import type { AdminLibrary, AdminPlugin } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("EmbyMigrationPluginConfig", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("guides an admin through the migration in three explicit steps", async () => {
    const testSource = vi.spyOn(api, "testAdminEmbyMigration").mockResolvedValue({
      serverName: "家庭 Emby",
      productName: "Emby Server",
      version: "4.8.10",
      serverId: "server-1",
      historyCapability: "ITEM_STATE",
    });
    const sourceUsers = vi.spyOn(api, "adminEmbyMigrationSourceUsers").mockResolvedValue({
      users: [
        { id: "user-1", name: "Alice", isDisabled: false, isAdministrator: false },
        { id: "user-2", name: "Bob", isDisabled: true, isAdministrator: false },
      ],
      total: 2,
      page: 1,
      pageSize: 100,
    });
    const migrationJob = {
        id: "job-1",
        sourceLabel: "emby.local",
        sourceBaseUrl: "http://emby.local:8096/",
        status: "PENDING",
        phase: "TESTING",
        dryRun: false,
        mergePolicy: "MERGE",
        scope: { userProfile: false, libraryAccess: false, itemState: true, itemStateFilters: ["FAVORITE"], personFavorites: false, targetLibraryIds: ["library-a"] },
        historyCapability: "ITEM_STATE",
        processedCount: 0,
        totalCount: 0,
        matchedCount: 0,
        skippedCount: 0,
        failedCount: 0,
        cancelRequested: false,
        error: null,
    } as const;
    const createJob = vi.spyOn(api, "createAdminEmbyMigration").mockResolvedValue({ job: migrationJob });
    const targetLibraries: AdminLibrary[] = [{
      id: "library-a",
      name: "电影库",
      kind: "MOVIE",
      isEnabled: true,
      realtimeWatchEnabled: false,
      realtimeMetadataAutoMatchEnabled: false,
      roots: [],
    }];
    const adminLibraries = vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: targetLibraries });
    vi.spyOn(api, "adminEmbyMigration").mockResolvedValue({ job: migrationJob });
    vi.spyOn(api, "adminEmbyMigrationUsers").mockResolvedValue({ users: [], page: 1, pageSize: 50 });
    vi.spyOn(api, "adminEmbyMigrationMatches").mockResolvedValue({
      matches: [
        {
          jobId: "job-1",
          embyItemId: "emby-episode-1",
          embyItemType: "Episode",
          luxItemId: "lux-episode-1",
          matchMethod: "EPISODE_KEY",
          confidence: 95,
          status: "MATCHED",
          detail: {
            title: "第十集",
            luxTitle: "第十集",
            luxSeriesTitle: "西游记",
            luxSeasonNumber: 2,
            luxEpisodeNumber: 10,
          },
        },
        {
          jobId: "job-1",
          embyItemId: "emby-movie-1",
          embyItemType: "Movie",
          luxItemId: "lux-movie-1",
          matchMethod: "TMDB_ID",
          confidence: 100,
          status: "MATCHED",
          detail: { title: "星际穿越", luxTitle: "星际穿越" },
        },
      ],
      page: 1,
      pageSize: 50,
    });
    vi.spyOn(api, "adminEmbyMigrationImports").mockResolvedValue({ imports: [], page: 1, pageSize: 50 });
    vi.spyOn(api, "adminEmbyMigrationPersonFavorites").mockResolvedValue({ personFavorites: [], page: 1, pageSize: 50 });
    vi.spyOn(api, "updateAdminPluginConfig").mockResolvedValue({ plugin: {} as AdminPlugin });
    vi.spyOn(api, "adminEmbyMigrations").mockResolvedValue({ jobs: [], total: 0, page: 1, pageSize: 20 });
    const plugin: AdminPlugin = {
      id: "org.lux.emby-migration",
      name: "Emby 迁移助手",
      description: "仅支持 Emby 到 Lux。",
      category: "MIGRATION",
      version: "1.0.0",
      runtime: "process",
      capabilities: ["migration.emby"],
      status: "READY",
      running: true,
      lastError: null,
      installed: true,
      enabled: true,
      configured: true,
      available: true,
      configurable: true,
      configFields: [
        { key: "baseUrl", label: "Emby 地址", type: "text", required: true, sensitive: false },
        { key: "apiKey", label: "Emby API Key", type: "password", required: true, sensitive: true },
        { key: "allowPrivateNetwork", label: "允许连接局域网地址", type: "toggle", required: false, sensitive: false },
      ],
      configValues: { baseUrl: "http://emby.local:8096", allowPrivateNetwork: false },
      configSource: "PLUGIN_CONFIG",
    };

    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container);
    act(() => {
      root.render(
        <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
          <MemoryRouter><EmbyMigrationPluginConfig plugin={plugin} /></MemoryRouter>
        </QueryClientProvider>,
      );
    });

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("连接 Emby"));
    });
    const initialPanel = container.querySelector<HTMLElement>('[data-testid="emby-migration-step-panel"]');
    expect(initialPanel?.dataset.step).toBe("1");
    expect(initialPanel?.textContent).toContain("连接 Emby");
    expect(initialPanel?.querySelector("#emby-migration-users-heading")).toBeNull();
    expect(container.textContent).toContain("第 1 步");
    expect(container.textContent).toContain("第 2 步");
    expect(container.textContent).toContain("第 3 步");
    expect(container.querySelector("details.lux-emby-advanced-options")).toBeNull();
    expect(container.querySelector("#emby-plugin-base-url")).not.toBeNull();
    expect(container.querySelector("#emby-plugin-api-key")).not.toBeNull();
    expect(container.querySelector("#emby-migration-base-url")).toBeNull();
    act(() => {
      container.querySelector<HTMLButtonElement>('button[aria-label="保存并测试 Emby 连接"]')?.click();
    });
    await act(async () => {
      await vi.waitFor(() => expect(api.updateAdminPluginConfig).toHaveBeenCalledWith("org.lux.emby-migration", {
        baseUrl: "http://emby.local:8096",
        allowPrivateNetwork: false,
      }));
      await vi.waitFor(() => expect(testSource).toHaveBeenCalledWith());
    });
    expect(container.textContent).toContain("家庭 Emby");
    expect(container.textContent).toContain("仅迁移条目状态，历史时间线不可用");
    expect(container.querySelector<HTMLElement>('[data-testid="emby-migration-step-panel"]')?.dataset.step).toBe("2");
    await act(async () => {
      await vi.waitFor(() => expect(sourceUsers).toHaveBeenCalledWith(1));
    });
    expect(adminLibraries).not.toHaveBeenCalled();
    expect(api.adminEmbyMigrations).not.toHaveBeenCalled();
    expect(container.textContent).toContain("Alice");
    expect(container.querySelector<HTMLButtonElement>('button[aria-label="下一步：确认迁移"]')?.disabled).toBe(true);
    act(() => {
      container.querySelector<HTMLInputElement>('input[aria-label="选择 Emby 用户 Alice"]')?.click();
    });
    expect(container.querySelector<HTMLButtonElement>('button[aria-label="下一步：确认迁移"]')?.disabled).toBe(true);
    act(() => {
      container.querySelector<HTMLInputElement>('input[aria-label="选择迁移媒体状态收藏"]')?.click();
    });
    await act(async () => {
      await vi.waitFor(() => expect(adminLibraries).toHaveBeenCalledWith());
      await vi.waitFor(() => expect(container.querySelector('input[aria-label="选择目标 Lux 媒体库 电影库"]')).not.toBeNull());
    });
    expect(container.querySelector<HTMLButtonElement>('button[aria-label="下一步：确认迁移"]')?.disabled).toBe(true);
    act(() => {
      container.querySelector<HTMLInputElement>('input[aria-label="选择目标 Lux 媒体库 电影库"]')?.click();
    });
    expect(container.querySelector<HTMLButtonElement>('button[aria-label="下一步：确认迁移"]')?.disabled).toBe(false);

    act(() => {
      Array.from(container.querySelectorAll<HTMLButtonElement>("button")).find((button) => button.textContent?.trim() === "上一步")?.click();
    });
    expect(container.querySelector<HTMLElement>('[data-testid="emby-migration-step-panel"]')?.dataset.step).toBe("1");
    act(() => {
      container.querySelector<HTMLButtonElement>('button[aria-label="保存并测试 Emby 连接"]')?.click();
    });
    await act(async () => {
      await vi.waitFor(() => expect(testSource).toHaveBeenCalledTimes(2));
      await vi.waitFor(() => expect(container.querySelector<HTMLElement>('[data-testid="emby-migration-step-panel"]')?.dataset.step).toBe("2"));
    });
    expect(container.querySelector<HTMLInputElement>('input[aria-label="选择 Emby 用户 Alice"]')?.checked).toBe(true);

    act(() => container.querySelector<HTMLButtonElement>('button[aria-label="下一步：确认迁移"]')?.click());
    expect(container.querySelector<HTMLElement>('[data-testid="emby-migration-step-panel"]')?.dataset.step).toBe("3");
    expect(container.textContent).toContain("确认并开始");
    expect(container.textContent).toContain("合并");
    expect(container.textContent).toContain("覆盖");
    expect(container.textContent).toContain("跳过");
    expect(container.textContent).toContain("媒体状态");
    expect(container.textContent).toContain("电影库");
    act(() => container.querySelector<HTMLButtonElement>('button[aria-label="开始 Emby 迁移"]')?.click());
    await act(async () => {
      await vi.waitFor(() => expect(createJob).toHaveBeenCalledWith({
        dryRun: false,
        mergePolicy: "MERGE",
        embyUserIds: ["user-1"],
        scope: { userProfile: false, libraryAccess: false, itemState: true, itemStateFilters: ["FAVORITE"], personFavorites: false, targetLibraryIds: ["library-a"] },
      }));
    });
    expect(container.textContent).toContain("迁移任务已创建");
    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("当前任务"));
    });
    expect(api.adminEmbyMigrationUsers).not.toHaveBeenCalled();
    act(() => {
      const details = container.querySelector<HTMLDetailsElement>("details.lux-emby-reports");
      if (details) {
        details.open = true;
        details.dispatchEvent(new Event("toggle", { bubbles: true }));
      }
    });
    act(() => {
      const button = Array.from(container.querySelectorAll("button")).find((candidate) => candidate.textContent?.includes("媒体匹配"));
      button?.click();
    });
    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("Lux 媒体：西游记 · 第 2 季 · 第 10 集 · 第十集"));
    });
    expect(container.textContent).toContain("Lux 媒体：西游记 · 第 2 季 · 第 10 集 · 第十集");
    expect(container.textContent).toContain("Lux 媒体：星际穿越");
    expect(container.textContent).not.toContain("Lux 媒体：lux-episode-1");
    act(() => {
      const button = Array.from(container.querySelectorAll("button")).find((candidate) => candidate.textContent?.includes("人物收藏"));
      button?.click();
    });
    await act(async () => {
      await vi.waitFor(() => expect(api.adminEmbyMigrationPersonFavorites).toHaveBeenCalledWith("job-1", 1));
    });

    // Changing the source connection must invalidate the old migration scope;
    // otherwise a later submit could silently migrate stale users or libraries.
    act(() => {
      Array.from(container.querySelectorAll<HTMLButtonElement>("button"))
        .find((button) => button.textContent?.trim() === "上一步")
        ?.click();
    });
    expect(container.querySelector<HTMLElement>('[data-testid="emby-migration-step-panel"]')?.dataset.step).toBe("2");
    act(() => {
      Array.from(container.querySelectorAll<HTMLButtonElement>("button"))
        .find((button) => button.textContent?.trim() === "上一步")
        ?.click();
    });
    const changedBaseUrl = container.querySelector<HTMLInputElement>("#emby-plugin-base-url");
    expect(changedBaseUrl).not.toBeNull();
    act(() => {
      if (changedBaseUrl) {
        const valueSetter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
        valueSetter?.call(changedBaseUrl, "http://new-emby.local:8096");
        changedBaseUrl.dispatchEvent(new Event("input", { bubbles: true }));
        changedBaseUrl.dispatchEvent(new Event("change", { bubbles: true }));
      }
    });
    expect(container.querySelector<HTMLElement>('[data-testid="emby-migration-step-panel"]')?.dataset.step).toBe("1");
    act(() => {
      container.querySelector<HTMLButtonElement>('button[aria-label="保存并测试 Emby 连接"]')?.click();
    });
    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector<HTMLElement>('[data-testid="emby-migration-step-panel"]')?.dataset.step).toBe("2"));
    });
    expect(container.querySelector<HTMLInputElement>('input[aria-label="选择 Emby 用户 Alice"]')?.checked).toBe(false);
    expect(container.querySelector<HTMLInputElement>('input[aria-label="选择迁移媒体状态收藏"]')?.checked).toBe(false);
  });
});
