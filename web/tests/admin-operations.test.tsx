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

  it("shows active metadata jobs and translates job and audit labels", async () => {
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
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [] });
    vi.spyOn(api, "adminScheduledTasks").mockResolvedValue({ scheduledTasks: [], total: 0 });
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

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("元数据匹配"));
    });
    expect(container.textContent).toContain("运行中");
    expect(container.textContent).toContain("开始整库元数据匹配");
    expect(container.textContent).not.toContain("TMDb 匹配");
    expect(container.textContent).not.toContain("METADATA_REIDENTIFY_STARTED");
    const cancelButton = container.querySelector<HTMLButtonElement>('button[aria-label="取消任务"]');
    expect(cancelButton).not.toBeNull();
    act(() => cancelButton?.click());
    await act(async () => {
      await vi.waitFor(() => expect(cancelMetadata).toHaveBeenCalledWith("metadata-job-1"));
    });
  });

  it("shows all saved schedules and lets the form target a global schedule", async () => {
    vi.spyOn(api, "adminJobs").mockResolvedValue({ jobs: [] });
    vi.spyOn(api, "adminMetadataReidentifyJobs").mockResolvedValue({ jobs: [] });
    vi.spyOn(api, "adminLogs").mockResolvedValue({ events: [] });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [{ id: "library-1", name: "电影库", kind: "MOVIE", isEnabled: true, realtimeWatchEnabled: false, roots: [] }] });
    const updateSchedule = vi.spyOn(api, "updateAdminScheduledTask").mockResolvedValue({
      scheduledTask: {
        ownerType: "LIBRARY",
        ownerId: "library-1",
        ownerName: "电影库",
        taskType: "INCREMENTAL_SCAN",
        schedule: "interval:1h",
        isEnabled: true,
      },
    });
    vi.spyOn(api, "adminScheduledTasks").mockResolvedValue({
      scheduledTasks: [{
        ownerType: "LIBRARY",
        ownerId: "library-1",
        ownerName: "电影库",
        taskType: "INCREMENTAL_SCAN",
        schedule: "interval:30s",
        isEnabled: true,
        resourceLimit: {},
        createdAt: 1_700_000_000,
        updatedAt: 1_700_000_000,
      }],
      total: 1,
    });

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

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("计划任务"));
    });
    expect(container.textContent).toContain("电影库");
    expect(container.textContent).toContain("增量扫描");
    expect(container.textContent).toContain("interval:30s");
    expect(container.querySelector("select[name='schedule-owner']")).not.toBeNull();
    expect(container.querySelector("select[name='schedule-owner'] option[value='GLOBAL:global']")?.textContent).toBe("全局");

    const ownerSelect = container.querySelector("select[name='schedule-owner']") as HTMLSelectElement;
    const scheduleInput = container.querySelector("input[name='schedule-expression']") as HTMLInputElement;
    act(() => {
      Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")?.set?.call(ownerSelect, "LIBRARY:library-1");
      ownerSelect.dispatchEvent(new Event("change", { bubbles: true }));
    });
    expect(ownerSelect.value).toBe("LIBRARY:library-1");
    act(() => {
      Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")?.set?.call(ownerSelect, "GLOBAL:global");
      ownerSelect.dispatchEvent(new Event("change", { bubbles: true }));
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(scheduleInput, "interval:1h");
      scheduleInput.dispatchEvent(new Event("input", { bubbles: true }));
      scheduleInput.dispatchEvent(new Event("change", { bubbles: true }));
    });
    await act(async () => {
      (container.querySelector(".lux-admin-schedule-form button[type='submit']") as HTMLButtonElement).click();
      await vi.waitFor(() => expect(updateSchedule).toHaveBeenCalledWith({
        ownerType: "GLOBAL",
        ownerId: "global",
        taskType: "INCREMENTAL_SCAN",
        schedule: "interval:1h",
        isEnabled: true,
      }));
    });
  });
});
