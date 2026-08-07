// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AdminOperationsPage } from "../src/features/admin/AdminOperationsPage";
import { api } from "../src/lib/api/client";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("AdminOperationsPage", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("separates registered tasks, runtime records, and redacted audit logs", async () => {
    vi.spyOn(api, "adminJobs").mockResolvedValue({ jobs: [] });
    const cancelMetadata = vi.spyOn(api, "cancelMetadataReidentify").mockResolvedValue(undefined);
    vi.spyOn(api, "adminMetadataReidentifyJobs").mockResolvedValue({
      jobs: [{
        id: "metadata-job-1",
        status: "RUNNING",
        mode: "REIDENTIFY",
        processedCount: 3,
        totalCount: 10,
        error: null,
        createdAt: 1_700_000_000,
      }],
    });
    vi.spyOn(api, "adminLogs").mockResolvedValue({
      events: [{
        id: "audit-1",
        eventType: "METADATA_REIDENTIFY_STARTED",
        actorUsername: "admin",
        targetType: "library",
        targetId: "library-1",
        createdAt: 1_700_000_000,
      }],
    });
    vi.spyOn(api, "adminScheduledTasks").mockResolvedValue({ scheduledTasks: [], total: 0 });
    renderPage();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("已注册任务"));
    });
    expect(container.textContent).toContain("还没有注册任务");

    act(() => container.querySelector<HTMLButtonElement>('button[role="tab"]:nth-child(2)')?.click());
    expect(container.textContent).toContain("整库元数据匹配");
    expect(container.textContent).toContain("运行中");
    const cancelButton = container.querySelector<HTMLButtonElement>('button[aria-label="取消任务"]');
    expect(cancelButton).not.toBeNull();
    act(() => cancelButton?.click());
    await act(async () => {
      await vi.waitFor(() => expect(cancelMetadata).toHaveBeenCalledWith("metadata-job-1"));
    });

    act(() => container.querySelector<HTMLButtonElement>('button[role="tab"]:nth-child(3)')?.click());
    expect(container.textContent).toContain("开始整库元数据匹配");
    expect(container.textContent).not.toContain("METADATA_REIDENTIFY_STARTED");
  });

  it("edits an existing registered task without exposing a task creation form", async () => {
    vi.spyOn(api, "adminJobs").mockResolvedValue({ jobs: [] });
    vi.spyOn(api, "adminMetadataReidentifyJobs").mockResolvedValue({ jobs: [] });
    vi.spyOn(api, "adminLogs").mockResolvedValue({ events: [] });
    const updateSchedule = vi.spyOn(api, "updateAdminScheduledTask").mockResolvedValue({
      scheduledTask: {
        ownerType: "LIBRARY",
        ownerId: "library-1",
        ownerName: "电影库",
        taskType: "INCREMENTAL_SCAN",
        name: "扫描媒体文件夹",
        schedule: "interval:1h",
        isEnabled: true,
      },
    });
    vi.spyOn(api, "adminScheduledTasks").mockResolvedValue({
      scheduledTasks: [{
        id: "LIBRARY:library-1:INCREMENTAL_SCAN",
        ownerType: "LIBRARY",
        ownerId: "library-1",
        ownerName: "电影库",
        taskType: "INCREMENTAL_SCAN",
        name: "扫描媒体文件夹",
        description: "按计划检查媒体库根路径中的新增和变更文件。",
        sourceType: "SYSTEM",
        schedule: null,
        isEnabled: false,
      }],
      total: 1,
      page: 1,
      pageSize: 100,
    });
    renderPage();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("扫描媒体文件夹"));
    });
    expect(container.textContent).toContain("系统注册");
    expect(container.textContent).toContain("未配置计划");
    expect(container.textContent).not.toContain("新增任务");

    act(() => container.querySelector<HTMLButtonElement>('button[aria-label="编辑扫描媒体文件夹"]')?.click());
    const input = container.querySelector<HTMLInputElement>("input[id^='schedule-LIBRARY']");
    const enabled = container.querySelector<HTMLInputElement>("input[id^='enabled-LIBRARY']");
    expect(input).not.toBeNull();
    expect(enabled).not.toBeNull();
    act(() => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(input, "interval:1h");
      input?.dispatchEvent(new Event("input", { bubbles: true }));
      enabled?.click();
    });
    await act(async () => {
      container.querySelector<HTMLButtonElement>(".lux-registered-task-editor button[type='submit']")?.click();
      await vi.waitFor(() => expect(updateSchedule).toHaveBeenCalledWith({
        ownerType: "LIBRARY",
        ownerId: "library-1",
        taskType: "INCREMENTAL_SCAN",
        schedule: "interval:1h",
        isEnabled: true,
      }));
    });
  });

  function renderPage() {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <AdminOperationsPage />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
  }
});
