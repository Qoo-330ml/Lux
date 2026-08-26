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
        startedAt: 1_700_000_001,
        finishedAt: 1_700_000_061,
    }] });
    const cancelMetadata = vi.spyOn(api, "cancelMetadataReidentify").mockResolvedValue(undefined);
    vi.spyOn(api, "adminMetadataReidentifyJobs").mockResolvedValue({
      jobs: [{
        id: "metadata-job-1",
        status: "RUNNING",
        mode: "REIDENTIFY",
        processedCount: 3,
        totalCount: 10,
        pendingCount: 2,
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
    expect(container.textContent).toContain("低匹配 2 项");
    expect(container.textContent).toContain("总耗时：1 分钟");
    const metadataRow = [...container.querySelectorAll<HTMLElement>(".lux-admin-job-row")]
      .find((row) => row.textContent?.includes("整库元数据匹配"));
    expect(metadataRow?.textContent).not.toContain("总耗时");
    expect(container.querySelector<HTMLAnchorElement>('a[href="/libraries/library-1?metadataStatus=pending"]')).not.toBeNull();
    expect(container.textContent).toContain("完成时间");
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

  it("shows per-item metadata failure details on demand", async () => {
    vi.spyOn(api, "adminJobs").mockResolvedValue({ jobs: [] });
    vi.spyOn(api, "adminMetadataReidentifyJobs").mockResolvedValue({
      jobs: [{
        id: "metadata-job-failed",
        status: "COMPLETED_WITH_ISSUES",
        mode: "FILL_MISSING",
        processedCount: 2,
        totalCount: 2,
        error: "ITEM_ISSUES",
        createdAt: 1_700_000_000,
        libraryId: "library-1",
      }],
    });
    const getJob = vi.spyOn(api, "adminMetadataReidentifyJob").mockResolvedValue({
      job: {
        id: "metadata-job-failed",
        status: "COMPLETED_WITH_ISSUES",
        mode: "FILL_MISSING",
        processedCount: 2,
        totalCount: 2,
        error: "ITEM_ISSUES",
        createdAt: 1_700_000_000,
        libraryId: "library-1",
        items: [
          { jobId: "metadata-job-failed", itemId: "movie-1", status: "FAILED", candidateCount: 1, error: "METADATA_IMAGE_WRITE_FAILED", updatedAt: 1_700_000_001 },
          { jobId: "metadata-job-failed", itemId: "movie-2", status: "FAILED", candidateCount: 0, error: "TMDB_UNAVAILABLE", updatedAt: 1_700_000_002 },
          { jobId: "metadata-job-failed", itemId: "movie-ok", status: "COMPLETED", candidateCount: 1, error: null, updatedAt: 1_700_000_003 },
          { jobId: "metadata-job-failed", itemId: "movie-unsafe", status: "FAILED", candidateCount: 0, error: "/media/secret?token=abc", updatedAt: 1_700_000_004 },
        ],
      },
    });
    vi.spyOn(api, "adminLogs").mockResolvedValue({ events: [] });
    vi.spyOn(api, "adminScheduledTasks").mockResolvedValue({ scheduledTasks: [], total: 0 });
    renderPage();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("已注册任务"));
    });
    act(() => container.querySelector<HTMLButtonElement>('button[role="tab"]:nth-child(2)')?.click());

    expect(container.textContent).toContain("部分条目处理失败");
    expect(container.textContent).toContain("失败原因");

    await act(async () => {
      container.querySelector<HTMLButtonElement>('button[aria-label="查看元数据仅补全问题详情"]')?.click();
      await vi.waitFor(() => expect(getJob).toHaveBeenCalledWith("metadata-job-failed"));
    });
    expect(container.textContent).toContain("问题条目 3 个");
    expect(container.textContent).toContain("图片下载或写回失败");
    expect(container.textContent).toContain("TMDb 服务暂时不可用");
    expect(container.textContent).toContain("movie-1");
    expect(container.textContent).not.toContain("movie-ok");
    expect(container.textContent).not.toContain("/media/secret?token=abc");
    expect(container.textContent).toContain("未提供可识别的错误码");
  });

  it("filters completed-with-issues runs without querying unsupported job endpoints", async () => {
    const unsupportedStatusError = (status?: string) => status === "COMPLETED_WITH_ISSUES"
      ? Promise.reject(new Error("任务状态无效"))
      : Promise.resolve({ jobs: [] });
    vi.spyOn(api, "adminJobs").mockImplementation(unsupportedStatusError);
    vi.spyOn(api, "adminStrmProbeJobs").mockImplementation(unsupportedStatusError);
    vi.spyOn(api, "adminChapterDetectionJobs").mockImplementation(unsupportedStatusError);
    vi.spyOn(api, "adminDanmakuMatchJobs").mockImplementation(unsupportedStatusError);
    vi.spyOn(api, "adminLibraryCoverJobs").mockImplementation(unsupportedStatusError);
    vi.spyOn(api, "adminMetadataReidentifyJobs").mockResolvedValue({
      jobs: [{
        id: "metadata-job-issues",
        status: "COMPLETED_WITH_ISSUES",
        mode: "FILL_MISSING",
        processedCount: 2,
        totalCount: 2,
        error: "ITEM_ISSUES",
        createdAt: 1_700_000_000,
        libraryId: "library-1",
      }],
    });
    vi.spyOn(api, "adminLogs").mockResolvedValue({ events: [] });
    vi.spyOn(api, "adminScheduledTasks").mockResolvedValue({ scheduledTasks: [], total: 0 });
    renderPage();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("已注册任务"));
    });
    act(() => container.querySelector<HTMLButtonElement>('button[role="tab"]:nth-child(2)')?.click());
    act(() => container.querySelector<HTMLButtonElement>('button[aria-label="运行记录状态"]')?.click());
    act(() => document.querySelector<HTMLButtonElement>('button[data-value="COMPLETED_WITH_ISSUES"]')?.click());

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("已完成，有问题"));
    });
    expect(container.querySelector("h1")?.textContent).not.toBe("任务数据加载失败");
    expect(container.textContent).toContain("元数据仅补全");
    expect(container.textContent).toContain("部分条目处理失败");
  });

  it("puts the newest runtime record first when timestamps are ISO strings", async () => {
    vi.spyOn(api, "adminJobs").mockResolvedValue({ jobs: [{
      id: "scan-job-1",
      libraryId: "library-1",
      jobType: "INCREMENTAL_SCAN",
      status: "COMPLETED",
      createdAt: "2026-08-15T09:00:00.000Z",
    }] });
    vi.spyOn(api, "adminMetadataReidentifyJobs").mockResolvedValue({ jobs: [{
      id: "metadata-job-1",
      libraryId: "library-1",
      status: "COMPLETED",
      mode: "REIDENTIFY",
      processedCount: 1,
      totalCount: 1,
      createdAt: "2026-08-15T10:00:00.000Z",
    }] });
    vi.spyOn(api, "adminLogs").mockResolvedValue({ events: [] });
    vi.spyOn(api, "adminScheduledTasks").mockResolvedValue({ scheduledTasks: [], total: 0 });
    renderPage();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("已注册任务"));
    });
    act(() => container.querySelector<HTMLButtonElement>('button[role="tab"]:nth-child(2)')?.click());

    const rows = [...container.querySelectorAll<HTMLElement>(".lux-admin-job-row")];
    expect(rows).toHaveLength(2);
    expect(rows[0]?.textContent).toContain("整库元数据匹配");
    expect(rows[1]?.textContent).toContain("实时增量扫描");
  });

  it("shows discovery progress and cancellation state immediately", async () => {
    const adminJobs = vi.spyOn(api, "adminJobs").mockResolvedValue({ jobs: [{
      id: "scan-job-1",
      libraryId: "library-1",
      jobType: "RECONCILE_LIBRARY",
      status: "RUNNING",
      processedCount: 0,
      totalCount: 0,
      discoveryCompleted: false,
      cancelRequested: false,
      createdAt: 1_700_000_001,
    }] });
    vi.spyOn(api, "adminMetadataReidentifyJobs").mockResolvedValue({ jobs: [] });
    vi.spyOn(api, "adminLogs").mockResolvedValue({ events: [] });
    vi.spyOn(api, "adminScheduledTasks").mockResolvedValue({ scheduledTasks: [], total: 0 });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [{ id: "library-1", name: "电影库" }] });
    renderPage();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("已注册任务"));
    });
    act(() => container.querySelector<HTMLButtonElement>('button[role="tab"]:nth-child(2)')?.click());
    expect(container.textContent).toContain("正在发现目录");
    expect(container.querySelector<HTMLElement>(".lux-job-progress.is-indeterminate")).not.toBeNull();
    expect(container.querySelector<HTMLButtonElement>('button[aria-label="取消任务"]')).not.toBeNull();

    act(() => {
      root.unmount();
      container.remove();
    });
    adminJobs.mockResolvedValue({ jobs: [{
      id: "scan-job-1",
      libraryId: "library-1",
      jobType: "RECONCILE_LIBRARY",
      status: "RUNNING",
      processedCount: 0,
      totalCount: 10,
      discoveryCompleted: true,
      cancelRequested: true,
      createdAt: 1_700_000_001,
    }] });
    renderPage();
    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("已注册任务"));
    });
    act(() => container.querySelector<HTMLButtonElement>('button[role="tab"]:nth-child(2)')?.click());
    expect(container.textContent).toContain("正在停止…");
    expect(container.querySelector<HTMLButtonElement>('button[aria-label="正在取消任务"]')?.disabled).toBe(true);
  });

  it("shows STRM probe progress and can cancel the active job", async () => {
    vi.spyOn(api, "adminJobs").mockResolvedValue({ jobs: [] });
    vi.spyOn(api, "adminMetadataReidentifyJobs").mockResolvedValue({ jobs: [] });
    vi.spyOn(api, "adminStrmProbeJobs").mockResolvedValue({ jobs: [{
      id: "strm-job-1",
      operationId: "operation-1",
      libraryId: "library-1",
      status: "RUNNING",
      processedCount: 2,
      totalCount: 10,
      cancelRequested: false,
    }] });
    const cancelStrm = vi.spyOn(api, "cancelStrmProbeJob").mockResolvedValue(undefined);
    vi.spyOn(api, "adminLogs").mockResolvedValue({ events: [] });
    vi.spyOn(api, "adminScheduledTasks").mockResolvedValue({ scheduledTasks: [], total: 0 });
    renderPage();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("已注册任务"));
    });
    act(() => container.querySelector<HTMLButtonElement>('button[role="tab"]:nth-child(2)')?.click());
    expect(container.textContent).toContain("STRM 媒体信息扫描");
    expect(container.textContent).toContain("STRM 媒体信息任务 · 媒体库：电影库");
    expect(container.textContent).toContain("2 / 10");
    expect(container.querySelector<HTMLElement>(".lux-job-progress span")?.getAttribute("style")).toContain("20%");

    act(() => container.querySelector<HTMLButtonElement>('button[aria-label="取消任务"]')?.click());
    await act(async () => {
      await vi.waitFor(() => expect(cancelStrm).toHaveBeenCalledWith("strm-job-1"));
    });
  });

  it("shows STRM probe retry actions and its audit event label", async () => {
    vi.spyOn(api, "adminJobs").mockResolvedValue({ jobs: [] });
    vi.spyOn(api, "adminMetadataReidentifyJobs").mockResolvedValue({ jobs: [] });
    vi.spyOn(api, "adminStrmProbeJobs").mockResolvedValue({ jobs: [{
      id: "strm-job-failed",
      operationId: "operation-1",
      libraryId: "library-1",
      status: "FAILED",
      processedCount: 10,
      totalCount: 10,
      error: "STRM_MEDIA_INFO_FAILED",
    }] });
    const retryStrm = vi.spyOn(api, "retryStrmProbeJob").mockResolvedValue({ job: {
      id: "strm-job-retry",
      operationId: "operation-2",
      libraryId: "library-1",
      status: "PENDING",
      processedCount: 0,
      totalCount: 10,
    } });
    vi.spyOn(api, "adminLogs").mockResolvedValue({ events: [{
      id: "strm-audit-1",
      eventType: "STRM_PROBE_STARTED",
      targetType: "strm_probe_operation",
      createdAt: 1_700_000_000,
    }] });
    vi.spyOn(api, "adminScheduledTasks").mockResolvedValue({ scheduledTasks: [], total: 0 });
    renderPage();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("已注册任务"));
    });
    act(() => container.querySelector<HTMLButtonElement>('button[role="tab"]:nth-child(2)')?.click());
    expect(container.querySelector<HTMLButtonElement>('button[aria-label="重试任务"]')).not.toBeNull();
    act(() => container.querySelector<HTMLButtonElement>('button[aria-label="重试任务"]')?.click());
    await act(async () => {
      await vi.waitFor(() => expect(retryStrm).toHaveBeenCalledWith("strm-job-failed"));
    });
    act(() => container.querySelector<HTMLButtonElement>('button[role="tab"]:nth-child(3)')?.click());
    expect(container.textContent).toContain("开始 STRM 媒体信息扫描");
  });

  it("includes chapter detection and danmaku jobs in the common run list", async () => {
    vi.spyOn(api, "adminJobs").mockResolvedValue({ jobs: [] });
    vi.spyOn(api, "adminMetadataReidentifyJobs").mockResolvedValue({ jobs: [] });
    vi.spyOn(api, "adminStrmProbeJobs").mockResolvedValue({ jobs: [] });
    vi.spyOn(api, "adminChapterDetectionJobs").mockResolvedValue({ jobs: [{
      id: "chapter-job-1",
      libraryId: "library-1",
      pluginId: "org.lux.chapter",
      status: "RUNNING",
      processedCount: 1,
      totalCount: 4,
    }] });
    vi.spyOn(api, "adminDanmakuMatchJobs").mockResolvedValue({ jobs: [{
      id: "danmaku-job-1",
      libraryId: "library-1",
      status: "FAILED",
      processedCount: 4,
      totalCount: 4,
    }] });
    const cancelChapter = vi.spyOn(api, "cancelChapterDetection").mockResolvedValue(undefined);
    const retryDanmaku = vi.spyOn(api, "retryDanmakuMatch").mockResolvedValue({ job: {
      id: "danmaku-job-2",
      libraryId: "library-1",
      status: "PENDING",
      processedCount: 0,
      totalCount: 4,
    } });
    vi.spyOn(api, "adminLogs").mockResolvedValue({ events: [] });
    vi.spyOn(api, "adminScheduledTasks").mockResolvedValue({ scheduledTasks: [], total: 0 });
    renderPage();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("已注册任务"));
    });
    act(() => container.querySelector<HTMLButtonElement>('button[role="tab"]:nth-child(2)')?.click());
    expect(container.textContent).toContain("片头片尾检测");
    expect(container.textContent).toContain("弹幕匹配");
    expect(container.textContent).toContain("1 / 4");
    act(() => container.querySelector<HTMLButtonElement>('button[aria-label="取消任务"]')?.click());
    await act(async () => await vi.waitFor(() => expect(cancelChapter).toHaveBeenCalledWith("chapter-job-1")));
    act(() => container.querySelector<HTMLButtonElement>('button[aria-label="重试任务"]')?.click());
    await act(async () => await vi.waitFor(() => expect(retryDanmaku).toHaveBeenCalledWith("danmaku-job-1")));
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
        schedule: "0 * * * *",
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
        description: "按计划校验媒体库索引与文件系统的一致性。",
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
    expect(container.textContent).toContain("未配置计划");
    expect(container.textContent).not.toContain("新增任务");

    act(() => container.querySelector<HTMLButtonElement>('button[aria-label="编辑全量校验媒体库"]')?.click());
    const input = container.querySelector<HTMLInputElement>("input[id^='schedule-LIBRARY']");
    const enabled = container.querySelector<HTMLInputElement>("input[id^='enabled-LIBRARY']");
    expect(input).not.toBeNull();
    expect(enabled).not.toBeNull();
    act(() => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(input, "0 * * * *");
      input?.dispatchEvent(new Event("input", { bubbles: true }));
      enabled?.click();
    });
    await act(async () => {
      container.querySelector<HTMLButtonElement>(".lux-registered-task-editor button[type='submit']")?.click();
      await vi.waitFor(() => expect(updateSchedule).toHaveBeenCalledWith({
        ownerType: "LIBRARY",
        ownerId: "library-1",
        taskType: "RECONCILIATION_SCAN",
        schedule: "0 * * * *",
        isEnabled: true,
      }));
    });
  });

  it("edits the global STRM task schedule without exposing a task toggle", async () => {
    vi.spyOn(api, "adminJobs").mockResolvedValue({ jobs: [] });
    vi.spyOn(api, "adminMetadataReidentifyJobs").mockResolvedValue({ jobs: [] });
    vi.spyOn(api, "adminLogs").mockResolvedValue({ events: [] });
    const updateSchedule = vi.spyOn(api, "updateAdminScheduledTask").mockResolvedValue({
      scheduledTask: {
        ownerType: "GLOBAL",
        ownerId: "global",
        ownerName: "全局",
        taskType: "STRM_MEDIA_INFO",
        name: "STRM 媒体信息扫描",
        schedule: "0 4 * * *",
        isEnabled: true,
      },
    });
    const runScheduledTask = vi.spyOn(api, "runAdminScheduledTask").mockResolvedValue({ status: "ACCEPTED", taskType: "STRM_MEDIA_INFO" });
    vi.spyOn(api, "adminScheduledTasks").mockResolvedValue({
      scheduledTasks: [{
        id: "GLOBAL:global:STRM_MEDIA_INFO",
        ownerType: "GLOBAL",
        ownerId: "global",
        ownerName: "全局",
        taskType: "STRM_MEDIA_INFO",
        name: "STRM 媒体信息扫描",
        description: "按插件配置扫描选定媒体库的 STRM 外部媒体信息。",
        sourceType: "PLUGIN",
        pluginId: "org.lux.strm-media-info",
        schedule: "0 3 * * *",
        isEnabled: true,
      }],
      total: 1,
      page: 1,
      pageSize: 100,
    });
    renderPage();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("STRM 媒体信息扫描"));
    });
    act(() => container.querySelector<HTMLButtonElement>('button[aria-label="编辑STRM 媒体信息扫描"]')?.click());
    const input = container.querySelector<HTMLInputElement>("input[id^='schedule-GLOBAL']");
    expect(input).not.toBeNull();
    expect(container.querySelector<HTMLInputElement>("input[id^='enabled-GLOBAL']")).toBeNull();
    act(() => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(input, "0 4 * * *");
      input?.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await act(async () => {
      container.querySelector<HTMLButtonElement>(".lux-registered-task-editor button[type='submit']")?.click();
      await vi.waitFor(() => expect(updateSchedule).toHaveBeenCalledWith({
        ownerType: "GLOBAL",
        ownerId: "global",
        taskType: "STRM_MEDIA_INFO",
        schedule: "0 4 * * *",
        isEnabled: undefined,
      }));
    });
    act(() => container.querySelector<HTMLButtonElement>('button[aria-label="立即执行STRM 媒体信息扫描"]')?.click());
    await act(async () => {
      await vi.waitFor(() => expect(runScheduledTask).toHaveBeenCalledWith({
        ownerType: "GLOBAL",
        ownerId: "global",
        taskType: "STRM_MEDIA_INFO",
      }));
    });
  });

  it("runs every registered task immediately through the common task worker", async () => {
    vi.spyOn(api, "adminJobs").mockResolvedValue({ jobs: [] });
    vi.spyOn(api, "adminMetadataReidentifyJobs").mockResolvedValue({ jobs: [] });
    vi.spyOn(api, "adminLogs").mockResolvedValue({ events: [] });
    const runScheduledTask = vi.spyOn(api, "runAdminScheduledTask").mockImplementation(async (input) => ({ status: "ACCEPTED", taskType: input.taskType }));
    vi.spyOn(api, "adminScheduledTasks").mockResolvedValue({
      scheduledTasks: [
        {
          id: "LIBRARY:library-1:RECONCILIATION_SCAN",
          ownerType: "LIBRARY",
          ownerId: "library-1",
          ownerName: "电影库",
          taskType: "RECONCILIATION_SCAN",
          name: "全量校验媒体库",
          description: "按计划校验媒体库索引与文件系统的一致性。",
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
      await vi.waitFor(() => expect(runScheduledTask).toHaveBeenCalledWith({ ownerType: "LIBRARY", ownerId: "library-1", taskType: "RECONCILIATION_SCAN" }));
    });

    act(() => container.querySelector<HTMLButtonElement>('button[aria-label="立即执行元数据刮削"]')?.click());
    await act(async () => {
      await vi.waitFor(() => expect(runScheduledTask).toHaveBeenCalledWith({ ownerType: "LIBRARY", ownerId: "library-1", taskType: "METADATA_PARSE" }));
    });

    act(() => container.querySelector<HTMLButtonElement>('button[aria-label="立即执行自动生成媒体库封面"]')?.click());
    await act(async () => {
      await vi.waitFor(() => expect(runScheduledTask).toHaveBeenCalledWith({ ownerType: "LIBRARY", ownerId: "library-1", taskType: "AUTO_LIBRARY_COVER" }));
    });
    expect(container.querySelector<HTMLButtonElement>('button[aria-label="编辑自动生成媒体库封面"]')).not.toBeNull();
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
    if (!vi.isMockFunction(api.adminStrmProbeJobs)) {
      vi.spyOn(api, "adminStrmProbeJobs").mockResolvedValue({ jobs: [] });
    }
    if (!vi.isMockFunction(api.adminChapterDetectionJobs)) {
      vi.spyOn(api, "adminChapterDetectionJobs").mockResolvedValue({ jobs: [] });
    }
    if (!vi.isMockFunction(api.adminDanmakuMatchJobs)) {
      vi.spyOn(api, "adminDanmakuMatchJobs").mockResolvedValue({ jobs: [] });
    }
    if (!vi.isMockFunction(api.adminLibraryCoverJobs)) {
      vi.spyOn(api, "adminLibraryCoverJobs").mockResolvedValue({ jobs: [] });
    }
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
