// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AdminUserDetailPage } from "../src/features/admin/AdminUserManagementPage";
import { api } from "../src/lib/api/client";
import type { AdminContentRestrictionIndexStatus, AdminUser } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("AdminUserDetailPage content restriction index status", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("shows partial index progress and explains the unfinished access behavior", async () => {
    const getStatus = vi.spyOn(api, "adminContentRestrictionIndexStatus").mockResolvedValue({
      totalItems: 8,
      indexedItems: 6,
      pendingItems: 2,
    });
    renderContentSection();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("6 / 8"));
    });

    expect(getStatus).toHaveBeenCalledOnce();
    expect(container.textContent).toContain("待就绪 2 条");
    expect(container.textContent).toContain("正在回填");
    expect(container.textContent).toContain("访问拦截尚未接入");
    const progress = container.querySelector("progress");
    expect(progress?.value).toBe(6);
    expect(progress?.max).toBe(8);
  });

  it("shows an empty-library state when no media items need indexing", async () => {
    vi.spyOn(api, "adminContentRestrictionIndexStatus").mockResolvedValue({
      totalItems: 0,
      indexedItems: 0,
      pendingItems: 0,
    });
    renderContentSection();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("目前没有可统计的媒体条目"));
    });

    expect(container.textContent).toContain("暂无媒体");
  });

  it("shows an accessible loading state before status data arrives", async () => {
    let resolveStatus!: (status: AdminContentRestrictionIndexStatus) => void;
    const pending = new Promise<AdminContentRestrictionIndexStatus>((resolve) => {
      resolveStatus = resolve;
    });
    vi.spyOn(api, "adminContentRestrictionIndexStatus").mockReturnValue(pending);
    renderContentSection();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("正在读取内容索引进度"));
    });
    expect(container.querySelector('[role="status"]')).toBeTruthy();

    await act(async () => resolveStatus({ totalItems: 1, indexedItems: 1, pendingItems: 0 }));
    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("1 / 1"));
    });

    expect(container.textContent).toContain("索引已就绪");
  });

  it("offers retry after an index status request fails", async () => {
    const getStatus = vi.spyOn(api, "adminContentRestrictionIndexStatus")
      .mockRejectedValueOnce(new Error("索引状态暂时不可用"))
      .mockResolvedValueOnce({ totalItems: 1, indexedItems: 1, pendingItems: 0 });
    renderContentSection();

    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector('[role="alert"]')?.textContent).toContain("索引状态暂时不可用"));
    });
    await act(async () => {
      findButton("重试").click();
      await vi.waitFor(() => expect(container.textContent).toContain("1 / 1"));
    });
    expect(getStatus).toHaveBeenCalledTimes(2);
  });

  function renderContentSection() {
    vi.spyOn(api, "adminUser").mockResolvedValue({ user: makeUser() });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => root.render(
      <QueryClientProvider client={queryClient}>
        <MemoryRouter initialEntries={["/admin/users/user-1?section=content"]}>
          <Routes>
            <Route path="/admin/users/:userId" element={<AdminUserDetailPage />} />
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>,
    ));
  }

  function findButton(label: string) {
    const button = [...container.querySelectorAll<HTMLButtonElement>("button")]
      .find((item) => item.textContent?.includes(label));
    expect(button, `button containing ${label}`).toBeTruthy();
    return button!;
  }
});

function makeUser(): AdminUser {
  return {
    id: "user-1",
    usernameNormalized: "viewer",
    displayName: "观众",
    isDisabled: false,
    isAdmin: false,
  };
}
