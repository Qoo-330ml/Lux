// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ScanActivityPopover } from "../src/features/activity/ScanActivityPopover";
import { api } from "../src/lib/api/client";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

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

  it("keeps a queued scan waiting and shows processed media without a provisional total", async () => {
    vi.spyOn(api, "adminTaskActivity").mockResolvedValue({
      activities: [{
        id: "scan-job-discovery",
        kind: "scan",
        taskType: "RECONCILE_LIBRARY",
        libraryId: "library-1",
        status: "PENDING",
        processedMediaCount: 0,
        processedCount: 10,
        totalCount: 10,
        discoveryCompleted: false,
        scanPhase: "INDEXING",
      }],
    });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({
      libraries: [{ id: "library-1", name: "电影库", kind: "MOVIE" }],
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
    expect(container.textContent).toContain("已处理 0 部电影");
    expect(container.textContent).not.toContain("10/10");
    expect(container.querySelector('[role="progressbar"]')?.getAttribute("aria-valuenow")).toBeNull();
  });

  it("counts full scans by series while incremental scans keep file totals", async () => {
    vi.spyOn(api, "adminTaskActivity").mockResolvedValue({
      activities: [
        {
          id: "series-scan",
          kind: "scan",
          taskType: "RECONCILE_LIBRARY",
          libraryId: "series-library",
          status: "RUNNING",
          processedMediaCount: 4,
          processedCount: 140,
          totalCount: 140,
          discoveryCompleted: true,
        },
        {
          id: "incremental-scan",
          kind: "scan",
          taskType: "INCREMENTAL_SCAN",
          libraryId: "series-library",
          status: "RUNNING",
          processedCount: 2,
          totalCount: 5,
          discoveryCompleted: true,
        },
      ],
    });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({
      libraries: [{ id: "series-library", name: "剧集库", kind: "SERIES" }],
    });
    container = document.createElement("div");
    document.body.append(container);
    root = mountPopover(container);

    await openPopover(container);

    const rows = [...container.querySelectorAll<HTMLElement>(".lux-scan-activity-row")];
    const fullScanRow = rows.find((row) => row.textContent?.includes("已处理 4 部剧"));
    const incrementalRow = rows.find((row) => row.textContent?.includes("2/5"));
    expect(fullScanRow?.textContent).toContain("已处理 4 部剧");
    expect(fullScanRow?.textContent).not.toContain("140");
    expect(fullScanRow?.querySelector('[role="progressbar"]')?.getAttribute("aria-valuenow")).toBeNull();
    expect(incrementalRow?.textContent).toContain("2/5");
    expect(incrementalRow?.querySelector('[role="progressbar"]')?.getAttribute("aria-valuenow")).toBe("40");
  });

  it("closes when clicking outside the activity popover", async () => {
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

});
