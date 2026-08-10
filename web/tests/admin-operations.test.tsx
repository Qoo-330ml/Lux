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
    vi.spyOn(api, "adminJobs").mockResolvedValue({ jobs: [{
      id: "scan-job-1",
      libraryId: "library-1",
      jobType: "INCREMENTAL_SCAN",
      status: "COMPLETED",
      processedCount: 1,
      totalCount: 1,
      createdAt: 1_700_000_001,
    }] });
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
        libraryId: "library-1",
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
    const runList = container.querySelector<HTMLElement>(".lux-admin-job-list");
    expect(runList?.textContent).toContain("媒体库：电影库");
    expect(runList?.textContent).not.toContain("library-1");
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

  it("exports a selected UTC log date range from the system log tab", async () => {
    vi.spyOn(api, "adminJobs").mockResolvedValue({ jobs: [] });
    vi.spyOn(api, "adminMetadataReidentifyJobs").mockResolvedValue({ jobs: [] });
    vi.spyOn(api, "adminLogs").mockResolvedValue({ events: [] });
    vi.spyOn(api, "adminScheduledTasks").mockResolvedValue({ scheduledTasks: [], total: 0 });
    const exportLogs = vi.spyOn(api, "exportAdminLogs").mockResolvedValue(
      new Blob(["zip"], { type: "application/zip" }),
    );
    Object.defineProperty(URL, "createObjectURL", {
      configurable: true,
      value: vi.fn(() => "blob:lux-logs"),
    });
    Object.defineProperty(URL, "revokeObjectURL", {
      configurable: true,
      value: vi.fn(),
    });
    vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(() => {});
    renderPage();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("已注册任务"));
    });
    act(() => container.querySelector<HTMLButtonElement>('button[role="tab"]:nth-child(3)')?.click());
    const from = container.querySelector<HTMLInputElement>('input[aria-label="日志起始日期"]');
    const to = container.querySelector<HTMLInputElement>('input[aria-label="日志结束日期"]');
    const exportButton = container.querySelector<HTMLButtonElement>('button[aria-label="导出日志 ZIP"]');
    expect(from).not.toBeNull();
    expect(to).not.toBeNull();
    expect(exportButton).not.toBeNull();

    await act(async () => {
      exportButton?.click();
      await vi.waitFor(() => expect(exportLogs).toHaveBeenCalledWith(from?.value, to?.value));
    });
    expect(container.textContent).toContain("日志已导出");

    act(() => {
      const valueSetter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
      valueSetter?.call(from, to?.value);
      from?.dispatchEvent(new Event("input", { bubbles: true }));
      from?.dispatchEvent(new Event("change", { bubbles: true }));
    });
    expect(container.querySelector<HTMLButtonElement>('button[aria-label="导出日志文件"]')).not.toBeNull();
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
        taskType: "RECONCILIATION_SCAN",
        name: "全量校验媒体库",
        isEnabled: true,
      },
    });
    vi.spyOn(api, "adminScheduledTasks").mockResolvedValue({
      scheduledTasks: [{
        id: "LIBRARY:library-1:RECONCILIATION_SCAN",
        ownerType: "LIBRARY",
        ownerId: "library-1",
        ownerName: "电影库",
        taskType: "RECONCILIATION_SCAN",
        name: "全量校验媒体库",
        description: "由宿主机 crontab 入队后校验媒体库索引与文件系统的一致性。",
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
      await vi.waitFor(() => expect(container.textContent).toContain("全量校验媒体库"));
    });
    expect(container.textContent).toContain("系统注册");
    expect(container.textContent).toContain("宿主机 crontab 管理执行时间");
    expect(container.textContent).not.toContain("新增任务");

    act(() => container.querySelector<HTMLButtonElement>('button[aria-label="编辑全量校验媒体库"]')?.click());
    const enabled = container.querySelector<HTMLInputElement>("input[id^='enabled-LIBRARY']");
    expect(enabled).not.toBeNull();
    act(() => {
      enabled?.click();
    });
    await act(async () => {
      container.querySelector<HTMLButtonElement>(".lux-registered-task-editor button[type='submit']")?.click();
      await vi.waitFor(() => expect(updateSchedule).toHaveBeenCalledWith({
        ownerType: "LIBRARY",
        ownerId: "library-1",
        taskType: "RECONCILIATION_SCAN",
        isEnabled: true,
      }));
    });
  });

  it("runs a registered task immediately with its task-specific worker", async () => {
    vi.spyOn(api, "adminJobs").mockResolvedValue({ jobs: [] });
    vi.spyOn(api, "adminMetadataReidentifyJobs").mockResolvedValue({ jobs: [] });
    vi.spyOn(api, "adminLogs").mockResolvedValue({ events: [] });
    const startScan = vi.spyOn(api, "startAdminScan").mockResolvedValue({
      job: {
        id: "scan-job-1",
        libraryId: "library-1",
        jobType: "RECONCILE_LIBRARY",
        status: "PENDING",
      },
    });
    const startMetadataRefresh = vi.spyOn(api, "startLibraryMetadataRefresh").mockResolvedValue({
      totalCount: 2,
      mode: "FILL_MISSING",
      job: {
        id: "metadata-job-1",
        status: "QUEUED",
        mode: "FILL_MISSING",
        processedCount: 0,
        totalCount: 2,
        createdAt: 1_700_000_000,
      },
    });
    const runAutoLibraryCover = vi.spyOn(api, "runAutoLibraryCover").mockResolvedValue({
      status: "QUEUED",
      taskType: "AUTO_LIBRARY_COVER",
    });
    vi.spyOn(api, "adminScheduledTasks").mockResolvedValue({
      scheduledTasks: [
        {
          id: "LIBRARY:library-1:RECONCILIATION_SCAN",
          ownerType: "LIBRARY",
          ownerId: "library-1",
          ownerName: "电影库",
          taskType: "RECONCILIATION_SCAN",
          name: "全量校验媒体库",
          description: "由宿主机 crontab 入队后校验媒体库索引与文件系统的一致性。",
          sourceType: "SYSTEM",
          schedule: null,
          isEnabled: false,
        },
        {
          id: "LIBRARY:library-1:METADATA_PARSE",
          ownerType: "LIBRARY",
          ownerId: "library-1",
          ownerName: "电影库",
          taskType: "METADATA_PARSE",
          name: "元数据刮削",
          description: "解析本地元数据，并在已配置时调用刮削插件补全内容。",
          sourceType: "SYSTEM",
          schedule: null,
          isEnabled: false,
        },
        {
          id: "LIBRARY:library-1:AUTO_LIBRARY_COVER",
          ownerType: "LIBRARY",
          ownerId: "library-1",
          ownerName: "电影库",
          taskType: "AUTO_LIBRARY_COVER",
          name: "自动生成媒体库封面",
          description: "首次达到至少 9 张海报后生成媒体库封面。",
          sourceType: "SYSTEM",
          schedule: null,
          isEnabled: false,
        },
      ],
      total: 3,
      page: 1,
      pageSize: 100,
    });
    renderPage();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("元数据刮削"));
    });

    act(() => container.querySelector<HTMLButtonElement>('button[aria-label="立即执行全量校验媒体库"]')?.click());
    await act(async () => {
      await vi.waitFor(() => expect(startScan).toHaveBeenCalledWith("library-1"));
    });

    act(() => container.querySelector<HTMLButtonElement>('button[aria-label="立即执行元数据刮削"]')?.click());
    await act(async () => {
      await vi.waitFor(() => expect(startMetadataRefresh).toHaveBeenCalledWith("library-1", "FILL_MISSING"));
    });

    act(() => container.querySelector<HTMLButtonElement>('button[aria-label="立即执行自动生成媒体库封面"]')?.click());
    await act(async () => {
      await vi.waitFor(() => expect(runAutoLibraryCover).toHaveBeenCalledWith("library-1"));
    });
    expect(container.querySelector<HTMLButtonElement>('button[aria-label="编辑自动生成媒体库封面"]')).toBeNull();
  });

  function renderPage() {
    container = document.createElement("div");
    document.body.append(container);
    vi.spyOn(api, "adminLibraries").mockResolvedValue({
      libraries: [{
        id: "library-1",
        name: "电影库",
        kind: "MOVIE",
        isEnabled: true,
        realtimeWatchEnabled: true,
        realtimeMetadataAutoMatchEnabled: false,
        roots: [],
      }],
    });
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
