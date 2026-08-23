// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AdminEmbyMigrationPage } from "../src/features/admin/AdminEmbyMigrationPage";
import { api } from "../src/lib/api/client";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("AdminEmbyMigrationPage", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("tests the source, starts a dry run, and shows the migration capability warning", async () => {
    const testSource = vi.spyOn(api, "testAdminEmbyMigration").mockResolvedValue({
      serverName: "家庭 Emby",
      productName: "Emby Server",
      version: "4.8.10",
      serverId: "server-1",
      historyCapability: "ITEM_STATE",
    });
    const createJob = vi.spyOn(api, "createAdminEmbyMigration").mockResolvedValue({
      job: {
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
      },
    });
    vi.spyOn(api, "adminEmbyMigrations").mockResolvedValue({ jobs: [], total: 0, page: 1, pageSize: 20 });

    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container);
    act(() => {
      root.render(
        <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
          <MemoryRouter>
            <AdminEmbyMigrationPage />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("Emby 迁移"));
    });
    const baseUrl = container.querySelector<HTMLInputElement>("#emby-migration-base-url");
    const apiKey = container.querySelector<HTMLInputElement>("#emby-migration-api-key");
    expect(baseUrl).not.toBeNull();
    expect(apiKey?.type).toBe("password");
    act(() => {
      if (baseUrl && apiKey) {
        baseUrl.value = "http://emby.local:8096";
        baseUrl.dispatchEvent(new Event("input", { bubbles: true }));
        apiKey.value = "test-key";
        apiKey.dispatchEvent(new Event("input", { bubbles: true }));
      }
      container.querySelector<HTMLButtonElement>('button[aria-label="测试 Emby 连接"]')?.click();
    });
    await act(async () => {
      await vi.waitFor(() => expect(testSource).toHaveBeenCalled());
    });
    expect(container.textContent).toContain("家庭 Emby");
    expect(container.textContent).toContain("完整历史播放时间线不可用");

    act(() => container.querySelector<HTMLButtonElement>('button[aria-label="开始 Emby 迁移"]')?.click());
    await act(async () => {
      await vi.waitFor(() => expect(createJob).toHaveBeenCalledWith(expect.objectContaining({ dryRun: true })));
    });
    expect(container.textContent).toContain("预览任务已创建");
  });
});
