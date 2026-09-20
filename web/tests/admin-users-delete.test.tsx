// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AdminUsersPage } from "../src/features/admin/AdminUsersPage";
import { api } from "../src/lib/api/client";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("AdminUsersPage user actions", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("shows the user list before library access options finish loading", async () => {
    vi.spyOn(api, "adminUsers").mockResolvedValue({ users: [{
      id: "user-1", usernameNormalized: "viewer", displayName: "观众", isDisabled: false,
      isAdmin: false, canManageServer: false, canRemoteAccess: false, canDownload: false,
    }] });
    vi.spyOn(api, "adminLibraries").mockReturnValue(new Promise(() => {}));
    renderPage();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("观众"));
    });

    expect(container.querySelector(".lux-admin-page-state")).toBeNull();
    expect(container.querySelector(".lux-admin-user-row")).toBeTruthy();
  });

  it("confirms before disabling and refreshes the list", async () => {
    const list = vi.spyOn(api, "adminUsers").mockResolvedValue({ users: [{
      id: "user-1", usernameNormalized: "viewer", displayName: "观众", isDisabled: false,
      isAdmin: false, canManageServer: false, canRemoteAccess: false, canDownload: false,
    }] });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [] });
    const disable = vi.spyOn(api, "disableAdminUser").mockResolvedValue({ user: {
      id: "user-1", usernameNormalized: "viewer", displayName: "观众", isDisabled: true, isAdmin: false,
    } });
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(false);
    renderPage();

    await act(async () => { await vi.waitFor(() => expect(container.textContent).toContain("观众")); });
    const button = container.querySelector<HTMLButtonElement>('button[aria-label="禁用观众账户"]');
    act(() => button?.click());
    expect(confirm).toHaveBeenCalledWith("确定要禁用账户“观众”吗？账户数据会保留，但用户将无法登录。");
    expect(disable).not.toHaveBeenCalled();

    confirm.mockReturnValue(true);
    await act(async () => {
      button?.click();
      await vi.waitFor(() => expect(disable).toHaveBeenCalledWith("user-1"));
      await vi.waitFor(() => expect(list).toHaveBeenCalledTimes(2));
    });
  });

  it("shows an enable button for disabled users and restores login", async () => {
    const list = vi.spyOn(api, "adminUsers").mockResolvedValue({ users: [{
      id: "user-1", usernameNormalized: "viewer", displayName: "观众", isDisabled: true,
      isAdmin: false, canManageServer: false, canRemoteAccess: false, canDownload: false,
    }] });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [] });
    const update = vi.spyOn(api, "updateAdminUser").mockResolvedValue({ user: {
      id: "user-1", usernameNormalized: "viewer", displayName: "观众", isDisabled: false,
      isAdmin: false,
    } });
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(false);
    renderPage();

    await act(async () => { await vi.waitFor(() => expect(container.textContent).toContain("观众")); });
    const button = container.querySelector<HTMLButtonElement>('button[aria-label="启用观众账户"]');
    expect(button).toBeTruthy();
    expect(button?.textContent).toContain("启用");
    expect(container.querySelector('button[aria-label="禁用观众账户"]')).toBeNull();
    act(() => button?.click());
    expect(confirm).toHaveBeenCalledWith("确定要启用账户“观众”吗？账户将恢复登录。");
    expect(update).not.toHaveBeenCalled();

    confirm.mockReturnValue(true);
    await act(async () => {
      button?.click();
      await vi.waitFor(() => expect(update).toHaveBeenCalledWith("user-1", { isDisabled: false }));
      await vi.waitFor(() => expect(list).toHaveBeenCalledTimes(2));
    });
  });

  it("shows a text disable button for enabled users", async () => {
    vi.spyOn(api, "adminUsers").mockResolvedValue({ users: [{
      id: "user-1", usernameNormalized: "viewer", displayName: "观众", isDisabled: false,
      isAdmin: false, canManageServer: false, canRemoteAccess: false, canDownload: false,
    }] });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [] });
    vi.spyOn(api, "disableAdminUser").mockResolvedValue({ user: {
      id: "user-1", usernameNormalized: "viewer", displayName: "观众", isDisabled: true,
      isAdmin: false,
    } });
    renderPage();

    await act(async () => { await vi.waitFor(() => expect(container.textContent).toContain("观众")); });
    const button = container.querySelector<HTMLButtonElement>('button[aria-label="禁用观众账户"]');
    expect(button?.textContent).toContain("禁用");
  });

  it("confirms before permanently deleting and refreshes the list", async () => {
    const list = vi.spyOn(api, "adminUsers").mockResolvedValue({ users: [{
      id: "user-1", usernameNormalized: "viewer", displayName: "观众", isDisabled: false,
      isAdmin: false, canManageServer: false, canRemoteAccess: false, canDownload: false,
    }] });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [] });
    const remove = vi.spyOn(api, "deleteAdminUser").mockResolvedValue();
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(false);
    renderPage();

    await act(async () => { await vi.waitFor(() => expect(container.textContent).toContain("观众")); });
    const button = container.querySelector<HTMLButtonElement>('button[aria-label="删除观众账户"]');
    act(() => button?.click());
    expect(confirm).toHaveBeenCalledWith("确定要永久删除账户“观众”吗？用户数据和迁移绑定将被删除，且无法恢复。");
    expect(remove).not.toHaveBeenCalled();

    confirm.mockReturnValue(true);
    await act(async () => {
      button?.click();
      await vi.waitFor(() => expect(remove).toHaveBeenCalledWith("user-1"));
      await vi.waitFor(() => expect(list).toHaveBeenCalledTimes(2));
    });
  });

  it("shows a delete failure", async () => {
    vi.spyOn(api, "adminUsers").mockResolvedValue({ users: [{
      id: "user-1", usernameNormalized: "viewer", displayName: "观众", isDisabled: false,
      isAdmin: false, canManageServer: false, canRemoteAccess: false, canDownload: false,
    }] });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [] });
    vi.spyOn(api, "deleteAdminUser").mockRejectedValue(new Error("最后一个管理员不能删除"));
    vi.spyOn(window, "confirm").mockReturnValue(true);
    renderPage();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("观众"));
      container.querySelector<HTMLButtonElement>('button[aria-label="删除观众账户"]')?.click();
      await vi.waitFor(() => expect(container.querySelector('[role="alert"]')?.textContent).toContain("删除失败：最后一个管理员不能删除"));
    });
  });

  function renderPage() {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => root.render(<QueryClientProvider client={queryClient}><AdminUsersPage /></QueryClientProvider>));
  }
});
