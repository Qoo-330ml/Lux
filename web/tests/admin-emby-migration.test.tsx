// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { EmbyMigrationPluginConfig } from "../src/features/admin/EmbyMigrationPluginConfig";
import { api } from "../src/lib/api/client";
import type { AdminPlugin } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("EmbyMigrationPluginConfig", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("keeps the full migration workspace inside the plugin configuration", async () => {
    const testSource = vi.spyOn(api, "testAdminEmbyMigration").mockResolvedValue({
      serverName: "家庭 Emby",
      productName: "Emby Server",
      version: "4.8.10",
      serverId: "server-1",
      historyCapability: "ITEM_STATE",
    });
    const migrationJob = {
        id: "job-1",
        sourceLabel: "emby.local",
        sourceBaseUrl: "http://emby.local:8096/",
        status: "PENDING",
        phase: "TESTING",
        dryRun: true,
        mergePolicy: "MERGE",
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
    vi.spyOn(api, "adminEmbyMigration").mockResolvedValue({ job: migrationJob });
    vi.spyOn(api, "adminEmbyMigrationUsers").mockResolvedValue({ users: [], page: 1, pageSize: 50 });
    vi.spyOn(api, "adminEmbyMigrationMatches").mockResolvedValue({ matches: [], page: 1, pageSize: 50 });
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
      await vi.waitFor(() => expect(container.textContent).toContain("连接设置"));
    });
    expect(container.querySelector("#emby-plugin-base-url")).not.toBeNull();
    expect(container.querySelector("#emby-plugin-api-key")).not.toBeNull();
    expect(container.querySelector("#emby-migration-base-url")).toBeNull();
    act(() => {
      container.querySelector<HTMLButtonElement>('button[aria-label="测试 Emby 连接"]')?.click();
    });
    await act(async () => {
      await vi.waitFor(() => expect(testSource).toHaveBeenCalledWith());
    });
    expect(container.textContent).toContain("家庭 Emby");
    expect(container.textContent).toContain("完整历史播放时间线不可用");

    act(() => container.querySelector<HTMLButtonElement>('button[aria-label="开始 Emby 迁移"]')?.click());
    await act(async () => {
      await vi.waitFor(() => expect(createJob).toHaveBeenCalledWith({ dryRun: true, mergePolicy: "MERGE" }));
    });
    expect(container.textContent).toContain("预览任务已创建");
    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("任务详情"));
    });
    act(() => {
      const button = Array.from(container.querySelectorAll("button")).find((candidate) => candidate.textContent?.includes("人物收藏"));
      button?.click();
    });
    await act(async () => {
      await vi.waitFor(() => expect(api.adminEmbyMigrationPersonFavorites).toHaveBeenCalledWith("job-1", 1));
    });
  });
});
