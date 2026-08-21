// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ScanActivityPopover } from "../src/features/activity/ScanActivityPopover";
import { api } from "../src/lib/api/client";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

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
});
