// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AdminUserDetailPage } from "../src/features/admin/AdminUserManagementPage";
import { api } from "../src/lib/api/client";
import type { AdminUser, AdminUserWebSessionPage } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("AdminUserDetailPage Web session management", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("shows safe session summaries and confirms revocation of only another session", async () => {
    const sessions: AdminUserWebSessionPage = {
      sessions: [
        { id: "current-session", createdAt: 1_700_000_000, updatedAt: 1_700_000_100, expiresAt: 1_702_000_000, lastSeenAt: 1_700_000_100, isCurrent: true },
        { id: "other-session", createdAt: 1_699_000_000, updatedAt: 1_699_000_100, expiresAt: 1_701_000_000, lastSeenAt: null, isCurrent: false },
      ],
      page: 1,
      pageSize: 20,
      totalCount: 2,
    };
    const refreshedSessions: AdminUserWebSessionPage = {
      ...sessions,
      sessions: [sessions.sessions[0]],
      totalCount: 1,
    };
    const list = vi.spyOn(api, "adminUserWebSessions")
      .mockResolvedValueOnce(sessions)
      .mockResolvedValueOnce(refreshedSessions);
    const revoke = vi.spyOn(api, "revokeAdminUserWebSession").mockResolvedValue(undefined);
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(false);
    renderDevices();
    await waitForSessionTable();

    expect(container.textContent).toContain("2 个有效 Web 登录");
    expect(container.textContent).toContain("当前会话");
    expect(container.textContent).toContain("最近活动未知");
    expect(container.textContent).not.toContain("session-token");
    const rows = [...container.querySelectorAll<HTMLTableRowElement>("tbody tr")];
    expect(rows).toHaveLength(2);
    expect(rows.find((row) => row.textContent?.includes("当前会话"))?.querySelector("button")).toBeNull();

    const revokeButton = findButton("撤销登录");
    act(() => revokeButton.click());
    expect(confirm).toHaveBeenCalledWith("撤销此 Web 登录后，该浏览器下次访问时需要重新登录。确定撤销吗？");
    expect(revoke).not.toHaveBeenCalled();

    confirm.mockReturnValue(true);
    await act(async () => {
      revokeButton.click();
      await vi.waitFor(() => expect(revoke).toHaveBeenCalledWith("user-1", "other-session"));
      await vi.waitFor(() => expect(list).toHaveBeenCalledTimes(2));
    });
    expect(container.textContent).toContain("Web 登录已撤销");
    expect(container.textContent).toContain("1 个有效 Web 登录");
  });

  it("paginates through active Web sessions", async () => {
    const firstPage: AdminUserWebSessionPage = {
      sessions: Array.from({ length: 20 }, (_, index) => makeSession(`session-${index + 1}`)),
      page: 1,
      pageSize: 20,
      totalCount: 21,
    };
    const secondPage: AdminUserWebSessionPage = {
      sessions: [makeSession("session-21")],
      page: 2,
      pageSize: 20,
      totalCount: 21,
    };
    const list = vi.spyOn(api, "adminUserWebSessions").mockImplementation(async (_userId, page = 1) => (
      page === 1 ? firstPage : secondPage
    ));
    renderDevices();
    await waitForSessionTable();
    expect(container.textContent).toContain("第 1 / 2 页");

    await act(async () => {
      findButton("下一页").click();
      await vi.waitFor(() => expect(list).toHaveBeenCalledWith("user-1", 2, 20));
      await vi.waitFor(() => expect(container.textContent).toContain("第 2 / 2 页"));
    });
    expect(container.querySelectorAll("tbody tr")).toHaveLength(1);
  });

  it("allows retry after a read error and shows an empty state", async () => {
    const list = vi.spyOn(api, "adminUserWebSessions")
      .mockRejectedValueOnce(new Error("temporary failure"))
      .mockResolvedValueOnce({ sessions: [], page: 1, pageSize: 20, totalCount: 0 });
    renderDevices();
    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector('[role="alert"]')?.textContent).toContain("temporary failure"));
    });

    await act(async () => {
      findButton("重试").click();
      await vi.waitFor(() => expect(list).toHaveBeenCalledTimes(2));
      await vi.waitFor(() => expect(container.textContent).toContain("当前没有有效的 Web 登录会话。"));
    });
  });

  function renderDevices() {
    vi.spyOn(api, "adminUser").mockResolvedValue({ user: makeUser() });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => root.render(
      <QueryClientProvider client={queryClient}>
        <MemoryRouter initialEntries={["/admin/users/user-1?section=devices"]}>
          <Routes>
            <Route path="/admin/users/:userId" element={<AdminUserDetailPage />} />
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>,
    ));
    return queryClient;
  }

  async function waitForSessionTable() {
    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector(".lux-admin-user-session-table")).toBeTruthy());
    });
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

function makeSession(id: string) {
  return {
    id,
    createdAt: 1_700_000_000,
    updatedAt: 1_700_000_100,
    expiresAt: 1_702_000_000,
    lastSeenAt: 1_700_000_100,
    isCurrent: false,
  };
}
