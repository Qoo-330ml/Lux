// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ScanActivityPopover } from "../src/features/activity/ScanActivityPopover";
import { api } from "../src/lib/api/client";
import type {
  AdminLibraryPipelineActivityPage,
  AdminLibraryPipelineStage,
  AdminLibraryPipelineStageName,
  AdminLibraryPipelineStatusCounts,
} from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

function emptyPipelinePage(): AdminLibraryPipelineActivityPage {
  return { page: 1, pageSize: 5, totalLibraries: 0, libraries: [] };
}

function mockPipelinePage(page = emptyPipelinePage()) {
  return vi.spyOn(api, "adminLibraryPipelineActivity").mockResolvedValue(page);
}

function counts(overrides: Partial<AdminLibraryPipelineStatusCounts> = {}) {
  return {
    queued: 0,
    running: 0,
    completed: 0,
    failed: 0,
    cancelled: 0,
    ...overrides,
  };
}

function stage(
  name: AdminLibraryPipelineStageName,
  planned: boolean,
  statusCounts = counts(),
  recentActivityAt: number | null = null,
): AdminLibraryPipelineStage {
  return { stage: name, planned, statusCounts, recentActivityAt };
}

function mountPopover(container: HTMLDivElement): Root {
  const root = createRoot(container);
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  act(() => {
    root.render(
      <QueryClientProvider client={queryClient}>
        <MemoryRouter>
          <ScanActivityPopover />
        </MemoryRouter>
      </QueryClientProvider>,
    );
  });
  return root;
}

async function openPopover(container: HTMLDivElement) {
  await act(async () => {
    await vi.waitFor(() => expect(container.querySelector("button[aria-label*='后台任务活动']")).not.toBeNull());
  });
  act(() => container.querySelector<HTMLButtonElement>("button[aria-label*='后台任务活动']")?.click());
}

describe("ScanActivityPopover", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("shows metadata and other background work, not only library scans", async () => {
    mockPipelinePage();
    vi.spyOn(api, "adminTaskActivity").mockResolvedValue({
      activities: [{
        id: "metadata-job-1",
        kind: "metadata",
        taskType: "FILL_MISSING",
        libraryId: "library-1",
        status: "RUNNING",
        processedCount: 3,
        totalCount: 10,
      }],
    });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({
      libraries: [{ id: "library-1", name: "电视剧库" }],
    });
    const cancel = vi.spyOn(api, "cancelMetadataReidentify").mockResolvedValue(undefined);
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <ScanActivityPopover />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });

    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector("button[aria-label*='后台任务活动']")).not.toBeNull());
    });
    act(() => container.querySelector<HTMLButtonElement>("button[aria-label*='后台任务活动']")?.click());
    expect(container.textContent).toContain("元数据刮削");
    expect(container.textContent).toContain("电视剧库");
    expect(container.textContent).toContain("3/10");

    act(() => container.querySelector<HTMLButtonElement>("button[aria-label='取消元数据刮削']")?.click());
    await act(async () => {
      await vi.waitFor(() => expect(cancel).toHaveBeenCalledWith("metadata-job-1"));
    });
  });

  it("shows the current media title and an accurate status for running non-scan work", async () => {
    mockPipelinePage();
    vi.spyOn(api, "adminTaskActivity").mockResolvedValue({
      activities: [{
        id: "chapter-job-1",
        kind: "chapter",
        taskType: "CHAPTER_DETECTION",
        libraryId: "library-1",
        status: "RUNNING",
        processedCount: 7448,
        totalCount: 25661,
        currentItem: "动漫 · S01E08 · 第八集",
      }],
    });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({
      libraries: [{ id: "library-1", name: "动漫" }],
    });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <ScanActivityPopover />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });

    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector("button[aria-label*='后台任务活动']")).not.toBeNull());
    });
    act(() => container.querySelector<HTMLButtonElement>("button[aria-label*='后台任务活动']")?.click());

    expect(container.textContent).toContain("处理中 · 动漫 · S01E08 · 第八集");
    expect(container.textContent).not.toContain("等待调度");
  });

  it("keeps a queued scan waiting and hides discovery's provisional total", async () => {
    mockPipelinePage();
    vi.spyOn(api, "adminTaskActivity").mockResolvedValue({
      activities: [{
        id: "scan-job-discovery",
        kind: "scan",
        taskType: "RECONCILE_LIBRARY",
        libraryId: "library-1",
        status: "PENDING",
        processedCount: 10,
        totalCount: 10,
        discoveryCompleted: false,
        scanPhase: "INDEXING",
      }],
    });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({
      libraries: [{ id: "library-1", name: "电影库" }],
    });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <ScanActivityPopover />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });

    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector("button[aria-label*='后台任务活动']")).not.toBeNull());
    });
    act(() => container.querySelector<HTMLButtonElement>("button[aria-label*='后台任务活动']")?.click());

    expect(container.textContent).toContain("等待调度");
    expect(container.textContent).toContain("发现中");
    expect(container.textContent).not.toContain("10/10");
  });

  it("closes when clicking outside the activity popover", async () => {
    mockPipelinePage();
    vi.spyOn(api, "adminTaskActivity").mockResolvedValue({
      activities: [{
        id: "scan-job-1",
        kind: "scan",
        taskType: "RECONCILE_LIBRARY",
        libraryId: "library-1",
        status: "RUNNING",
        processedCount: 3,
        totalCount: 10,
      }],
    });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({
      libraries: [{ id: "library-1", name: "电视剧库" }],
    });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <ScanActivityPopover />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });

    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector("button[aria-label*='后台任务活动']")).not.toBeNull());
    });
    act(() => container.querySelector<HTMLButtonElement>("button[aria-label*='后台任务活动']")?.click());
    expect(container.querySelector('[role="dialog"]')).not.toBeNull();

    act(() => container.querySelector<HTMLElement>('[role="dialog"]')?.dispatchEvent(new Event("pointerdown", { bubbles: true })));
    expect(container.querySelector('[role="dialog"]')).not.toBeNull();

    act(() => document.body.dispatchEvent(new Event("pointerdown", { bubbles: true })));
    expect(container.querySelector('[role="dialog"]')).toBeNull();
  });

  it("keeps completed-index postprocessing visible as active work", async () => {
    mockPipelinePage();
    vi.spyOn(api, "adminTaskActivity").mockResolvedValue({
      activities: [{
        id: "scan-job-postprocessing",
        kind: "scan",
        taskType: "RECONCILE_LIBRARY",
        libraryId: "library-1",
        status: "COMPLETED",
        processedCount: 1_000_000,
        totalCount: 1_000_000,
        currentItem: "媒体探测",
        scanPhase: "POSTPROCESSING",
      }],
    });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({
      libraries: [{ id: "library-1", name: "电影库" }],
    });
    const cancel = vi.spyOn(api, "cancelAdminJob").mockResolvedValue(undefined);
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <ScanActivityPopover />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });

    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector("button[aria-label*='后台任务活动']")).not.toBeNull());
    });
    act(() => container.querySelector<HTMLButtonElement>("button[aria-label*='后台任务活动']")?.click());
    expect(container.textContent).toContain("索引已完成，后处理进行中");
    expect(container.textContent).toContain("媒体探测");
    expect(container.textContent).not.toContain("100%");
    expect(container.querySelector<HTMLButtonElement>("button[aria-label='取消全量校验']")).toBeNull();
    expect(cancel).not.toHaveBeenCalled();
  });

  it("groups active stages by library and shows stage-local outcomes without fake percentages", async () => {
    vi.spyOn(api, "adminTaskActivity").mockResolvedValue({ activities: [] });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [] });
    mockPipelinePage({
      page: 1,
      pageSize: 5,
      totalLibraries: 2,
      libraries: [
        {
          libraryId: "library-1",
          libraryName: "电视剧长名称测试库",
          stages: [
            stage("INDEX", true, counts({ completed: 1 }), 1_700_000_000),
            stage("LOCAL_METADATA", true, counts({ running: 1, failed: 1 }), 1_700_000_010),
            stage("PROBE", true, counts({ queued: 4 }), 1_700_000_020),
            stage("THUMBNAIL", false),
            stage("AUTOMATIC_METADATA", true, counts({ failed: 2, cancelled: 1 }), 1_700_000_030),
          ],
        },
        {
          libraryId: "library-2",
          libraryName: "电影库",
          stages: [
            stage("INDEX", true, counts({ completed: 1 }), 1_700_000_000),
            stage("LOCAL_METADATA", false),
            stage("PROBE", false),
            stage("THUMBNAIL", false),
            stage("AUTOMATIC_METADATA", true, counts({ running: 1 }), 1_700_000_040),
          ],
        },
      ],
    });
    container = document.createElement("div");
    document.body.append(container);
    root = mountPopover(container);
    await openPopover(container);

    expect(container.textContent).toContain("电视剧长名称测试库");
    expect(container.textContent).toContain("电影库");
    expect(container.textContent).toContain("在线刮削");
    act(() => container.querySelector<HTMLElement>("summary[aria-label='展开媒体库 电视剧长名称测试库']")?.click());
    expect(container.textContent).toContain("索引扫描");
    expect(container.textContent).toContain("完成 1");
    expect(container.textContent).toContain("运行中 1");
    expect(container.textContent).toContain("排队 4");
    expect(container.textContent).toContain("未计划");
    expect(container.textContent).toContain("失败 2");
    expect(container.textContent).toContain("已取消 1");
    expect(container.querySelector('[role="progressbar"]')).toBeNull();

    act(() => container.querySelector<HTMLElement>("summary[aria-label='展开媒体库 电影库']")?.click());
    expect(container.textContent).toContain("运行中 1");
    expect(container.textContent).not.toContain("预计完成");
  });

  it("shows an empty pipeline state for a library with no planned work", async () => {
    vi.spyOn(api, "adminTaskActivity").mockResolvedValue({ activities: [] });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [] });
    mockPipelinePage({
      page: 1,
      pageSize: 5,
      totalLibraries: 1,
      libraries: [{
        libraryId: "library-empty",
        libraryName: "空媒体库",
        stages: [
          stage("INDEX", false),
          stage("LOCAL_METADATA", false),
          stage("PROBE", false),
          stage("THUMBNAIL", false),
          stage("AUTOMATIC_METADATA", false),
        ],
      }],
    });
    container = document.createElement("div");
    document.body.append(container);
    root = mountPopover(container);
    await openPopover(container);

    expect(container.textContent).toContain("目前没有正在运行的单项作业");
    expect(container.textContent).toContain("暂无计划");
    act(() => container.querySelector<HTMLElement>("summary[aria-label='展开媒体库 空媒体库']")?.click());
    expect(container.querySelectorAll('[aria-label$="未计划"]')).toHaveLength(5);
  });

  it("shows completed stages without marking the activity trigger as active", async () => {
    vi.spyOn(api, "adminTaskActivity").mockResolvedValue({ activities: [] });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [] });
    mockPipelinePage({
      page: 1,
      pageSize: 5,
      totalLibraries: 1,
      libraries: [{
        libraryId: "library-complete",
        libraryName: "已完成的媒体库",
        stages: [
          stage("INDEX", true, counts({ completed: 1 })),
          stage("LOCAL_METADATA", true, counts({ completed: 2 })),
          stage("PROBE", true, counts({ completed: 2 })),
          stage("THUMBNAIL", true, counts({ completed: 2 })),
          stage("AUTOMATIC_METADATA", true, counts({ completed: 2 })),
        ],
      }],
    });
    container = document.createElement("div");
    document.body.append(container);
    root = mountPopover(container);
    await openPopover(container);

    expect(container.querySelector(".lux-scan-activity-trigger.is-active")).toBeNull();
    act(() => container.querySelector<HTMLElement>("summary[aria-label='展开媒体库 已完成的媒体库']")?.click());
    expect(container.querySelectorAll('article[role="group"][aria-label*="完成"]').length).toBe(5);
    expect(container.textContent).not.toContain("预计完成");
  });

  it("paginates library summaries and lets the administrator refresh the current page", async () => {
    vi.spyOn(api, "adminTaskActivity").mockResolvedValue({ activities: [] });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [] });
    const firstPage: AdminLibraryPipelineActivityPage = {
      page: 1,
      pageSize: 5,
      totalLibraries: 6,
      libraries: Array.from({ length: 5 }, (_, index) => ({
        libraryId: `library-${index + 1}`,
        libraryName: `媒体库 ${index + 1}`,
        stages: [
          stage("INDEX", false),
          stage("LOCAL_METADATA", false),
          stage("PROBE", false),
          stage("THUMBNAIL", false),
          stage("AUTOMATIC_METADATA", false),
        ],
      })),
    };
    const secondPage: AdminLibraryPipelineActivityPage = {
      page: 2,
      pageSize: 5,
      totalLibraries: 6,
      libraries: [{
        libraryId: "library-6",
        libraryName: "媒体库 6",
        stages: [
          stage("INDEX", false),
          stage("LOCAL_METADATA", false),
          stage("PROBE", false),
          stage("THUMBNAIL", false),
          stage("AUTOMATIC_METADATA", false),
        ],
      }],
    };
    const request = vi.spyOn(api, "adminLibraryPipelineActivity")
      .mockResolvedValueOnce(firstPage)
      .mockResolvedValueOnce(secondPage)
      .mockResolvedValue(secondPage);
    container = document.createElement("div");
    document.body.append(container);
    root = mountPopover(container);
    await openPopover(container);
    expect(container.textContent).toContain("媒体库 1");
    expect(container.textContent).not.toContain("媒体库 6");

    act(() => container.querySelector<HTMLButtonElement>("button[aria-label='下一页媒体库']")?.click());
    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("媒体库 6"));
    });
    expect(container.textContent).toContain("2 / 2");
    expect(request).toHaveBeenLastCalledWith(2, 5);

    act(() => container.querySelector<HTMLButtonElement>("button[aria-label='刷新入库阶段']")?.click());
    await act(async () => {
      await vi.waitFor(() => expect(request).toHaveBeenCalledTimes(3));
    });
    expect(request).toHaveBeenLastCalledWith(2, 5);
  });

  it("shows an API error and retries without displaying raw error details", async () => {
    vi.spyOn(api, "adminTaskActivity").mockResolvedValue({ activities: [] });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [] });
    const request = vi.spyOn(api, "adminLibraryPipelineActivity")
      .mockRejectedValueOnce(new Error("DO_NOT_RENDER_PRIVATE_RESPONSE"))
      .mockResolvedValueOnce({
        page: 1,
        pageSize: 5,
        totalLibraries: 1,
        libraries: [{
          libraryId: "library-recovered",
          libraryName: "恢复后的媒体库",
          stages: [
            stage("INDEX", false),
            stage("LOCAL_METADATA", false),
            stage("PROBE", false),
            stage("THUMBNAIL", false),
            stage("AUTOMATIC_METADATA", false),
          ],
        }],
      });
    container = document.createElement("div");
    document.body.append(container);
    root = mountPopover(container);
    await openPopover(container);

    expect(container.querySelector('[role="alert"]')?.textContent).toContain("暂时无法读取媒体库入库阶段");
    expect(container.textContent).not.toContain("DO_NOT_RENDER_PRIVATE_RESPONSE");
    act(() => container.querySelector<HTMLButtonElement>("button[aria-label='重试读取入库阶段']")?.click());
    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("恢复后的媒体库"));
    });
    expect(request).toHaveBeenCalledTimes(2);
  });
});
