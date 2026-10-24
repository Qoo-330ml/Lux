// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider, notifyManager } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { MediaDetailPage } from "../src/features/detail/MediaDetailPage";
import { api } from "../src/lib/api/client";
import type { AdminItemMergeRecord, MediaItem } from "../src/lib/api/types";
import { mockPlaybackBootstrap } from "./player-test-helpers";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const mergeRecord: AdminItemMergeRecord = {
  id: "record-1",
  mergedItemId: "merged-item",
  mergedItemTitle: "另一个来源",
  itemType: "MOVIE",
  origin: "AUTO",
  mergedAt: 1_800_000_000,
  undoneAt: null,
  sourceCount: 2,
};

function detailItem(itemType = "MOVIE"): MediaItem {
  return {
    id: "primary-item",
    title: "主条目",
    itemType,
    mediaSources: [{ id: "source-1", isDefault: true, container: "mkv" }],
  };
}

describe("MediaMergeSourcesSection", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;

  beforeEach(() => {
    notifyManager.setScheduler(queueMicrotask);
    mockPlaybackBootstrap();
    vi.spyOn(api, "itemImages").mockResolvedValue({ images: [] });
    vi.spyOn(api, "playback").mockResolvedValue({});
    vi.spyOn(api, "children").mockResolvedValue({ items: [], total: 0, page: 1, pageSize: 100 });
  });

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
    vi.restoreAllMocks();
    notifyManager.setScheduler((callback) => setTimeout(callback, 0));
  });

  async function renderDetail(item: MediaItem) {
    if (root) act(() => root?.unmount());
    container?.remove();
    vi.spyOn(api, "item").mockResolvedValue(item);

    const rendered = document.createElement("div");
    container = rendered;
    document.body.append(rendered);
    root = createRoot(rendered);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={[`/items/${item.id}`]}>
            <Routes>
              <Route path="items/:itemId" element={<MediaDetailPage />} />
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    return rendered;
  }

  function mockSession(canManageServer: boolean) {
    vi.spyOn(api, "me").mockResolvedValue({
      user: { id: "user-1", usernameNormalized: "admin", displayName: "Admin", canManageServer },
    });
  }

  it("lists merged sources for administrators and undoes one after confirmation", async () => {
    mockSession(true);
    const merges = vi.spyOn(api, "adminItemMerges")
      .mockResolvedValueOnce({ records: [mergeRecord] })
      .mockResolvedValue({ records: [{ ...mergeRecord, undoneAt: 1_800_000_100 }] });
    const unmerge = vi.spyOn(api, "unmergeAdminItems").mockResolvedValue({
      primaryItemId: "primary-item",
      mergedItemId: "merged-item",
      libraryId: "library-1",
      itemType: "MOVIE",
      restoredItemIds: ["merged-item"],
      restoredSourceCount: 2,
      userStateRestored: false,
    });

    const rendered = await renderDetail(detailItem());
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(merges).toHaveBeenCalledWith("primary-item");
    const section = rendered.querySelector(".lux-detail-merge-sources");
    expect(section?.querySelector("h2")?.textContent).toBe("合并来源");
    expect(section?.textContent).toContain("另一个来源");
    expect(section?.textContent).toContain("自动合并");

    const trigger = section?.querySelector<HTMLButtonElement>('[data-action="unmerge-item"]');
    expect(trigger?.getAttribute("aria-label")).toBe("拆分已合并条目 另一个来源");
    await act(async () => trigger?.click());

    const confirm = rendered.querySelector<HTMLButtonElement>('[data-action="confirm-unmerge"]');
    expect(confirm).not.toBeNull();
    await act(async () => confirm?.click());
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(unmerge).toHaveBeenCalledWith({
      primaryItemId: "primary-item",
      mergedItemId: "merged-item",
    });
    expect(rendered.querySelector('[role="status"]')?.textContent).toContain("已拆分");
    expect(rendered.querySelector('[data-action="unmerge-item"]')).toBeNull();
  });

  it("does not request merge sources for non-administrators", async () => {
    mockSession(false);
    const merges = vi.spyOn(api, "adminItemMerges").mockResolvedValue({ records: [mergeRecord] });

    const rendered = await renderDetail(detailItem());
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(rendered.querySelector(".lux-detail-merge-sources")).toBeNull();
    expect(merges).not.toHaveBeenCalled();
  });

  it("shows a failure message when undoing a merge fails", async () => {
    mockSession(true);
    vi.spyOn(api, "adminItemMerges").mockResolvedValue({ records: [mergeRecord] });
    vi.spyOn(api, "unmergeAdminItems").mockRejectedValue(new Error("没有可撤销的合并记录"));

    const rendered = await renderDetail(detailItem());
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    await act(async () => rendered.querySelector<HTMLButtonElement>('[data-action="unmerge-item"]')?.click());
    await act(async () => rendered.querySelector<HTMLButtonElement>('[data-action="confirm-unmerge"]')?.click());
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(rendered.querySelector('[role="alert"]')?.textContent).toContain("拆分失败");
  });
});
