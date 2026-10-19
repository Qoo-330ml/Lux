// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AdminUsersPage } from "../src/features/admin/AdminUsersPage";
import { api } from "../src/lib/api/client";
import type { AdminSettings, AdminUser, AdminUserPermissionPreset } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const preset: AdminUserPermissionPreset = {
  id: "preset-basic",
  name: "普通用户",
  description: "基础播放权限",
  version: 1,
  isBuiltin: true,
  isAdmin: false,
  canManageServer: false,
  permissions: {
    libraryAccessMode: "ALL",
    libraryIds: [],
    canDownload: false,
    canPlay: true,
    maxSimultaneousPlaybacks: 2,
    allowRemux: true,
    allowAudioTranscode: false,
    allowVideoTranscode: false,
  },
};

function user(id: string, usernameNormalized: string, displayName: string): AdminUser {
  return {
    id,
    usernameNormalized,
    displayName,
    isDisabled: false,
    isAdmin: false,
    canManageServer: false,
    canRemoteAccess: false,
    canDownload: false,
  };
}

describe("AdminUsersPage batch permission preset actions", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("applies a preset only to explicitly selected users on the current page and shows per-user results", async () => {
    const alice = user("user-a", "alice", "爱丽丝");
    const bob = user("user-b", "bob", "鲍勃");
    const charlie = user("user-c", "charlie", "查理");
    const list = vi.spyOn(api, "adminUsers").mockResolvedValue({ users: [alice, bob, charlie], page: 1, pageSize: 20, totalCount: 4 });
    vi.spyOn(api, "adminUserPermissionPresets").mockResolvedValue({ presets: [preset] });
    vi.spyOn(api, "adminSettings").mockResolvedValue({ userDeletionGraceDays: 7 } as AdminSettings);
    const applyBatch = vi.spyOn(api, "applyAdminUserPermissionPresetBatch").mockResolvedValue({
      preset: { id: preset.id, name: preset.name, version: preset.version },
      results: [
        { userId: alice.id, success: true, errorCode: null },
        { userId: bob.id, success: false, errorCode: "LAST_MANAGER" },
      ],
    });
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    renderPage();

    await act(async () => { await vi.waitFor(() => expect(container.textContent).toContain("鲍勃")); });
    expect(container.textContent).toContain("仅对当前页勾选的用户操作");

    await act(async () => {
      container.querySelector<HTMLInputElement>('input[aria-label="选择爱丽丝"]')?.click();
      await vi.waitFor(() => expect(container.querySelector('option[value="preset-basic"]')).toBeTruthy());
      container.querySelector<HTMLInputElement>('input[aria-label="选择鲍勃"]')?.click();
      const select = container.querySelector<HTMLSelectElement>('select[aria-label="批量权限预设"]');
      if (select) {
        select.value = preset.id;
        select.dispatchEvent(new Event("change", { bubbles: true }));
      }
    });
    expect(container.textContent).toContain("当前页已选 2 人");

    await act(async () => {
      container.querySelector<HTMLButtonElement>('button[aria-label="批量应用权限预设"]')?.click();
      await vi.waitFor(() => expect(applyBatch).toHaveBeenCalledWith([alice.id, bob.id], preset.id));
      await vi.waitFor(() => expect(list).toHaveBeenCalledTimes(2));
    });
    expect(confirm).toHaveBeenCalledWith(expect.stringContaining("爱丽丝、鲍勃"));
    expect(confirm.mock.calls[0]?.[0]).not.toContain("查理");
    expect(container.textContent).toContain("爱丽丝：成功");
    expect(container.textContent).toContain("鲍勃：最后一个管理员不能失去管理权限");
    expect(container.textContent).toContain("权限预设“普通用户”的应用结果");
  });

  it("selects or clears only the users visible on the current page", async () => {
    vi.spyOn(api, "adminUsers").mockResolvedValue({
      users: [user("user-a", "alice", "爱丽丝"), user("user-b", "bob", "鲍勃")],
      page: 1,
      pageSize: 20,
      totalCount: 22,
    });
    vi.spyOn(api, "adminUserPermissionPresets").mockResolvedValue({ presets: [preset] });
    vi.spyOn(api, "adminSettings").mockResolvedValue({ userDeletionGraceDays: 7 } as AdminSettings);
    renderPage();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("鲍勃"));
      container.querySelector<HTMLButtonElement>('button[aria-label="全选本页"]')?.click();
    });
    expect(container.textContent).toContain("当前页已选 2 人");
    expect([...container.querySelectorAll<HTMLInputElement>(".lux-admin-user-select input")].every((input) => input.checked)).toBe(true);

    await act(async () => {
      container.querySelector<HTMLButtonElement>('button[aria-label="取消本页全选"]')?.click();
    });
    expect(container.textContent).toContain("当前页已选 0 人");
    expect([...container.querySelectorAll<HTMLInputElement>(".lux-admin-user-select input")].some((input) => input.checked)).toBe(false);
  });

  it("does not submit a batch when the confirmation is cancelled", async () => {
    vi.spyOn(api, "adminUsers").mockResolvedValue({ users: [user("user-a", "alice", "爱丽丝")], page: 1, pageSize: 20, totalCount: 1 });
    vi.spyOn(api, "adminUserPermissionPresets").mockResolvedValue({ presets: [preset] });
    vi.spyOn(api, "adminSettings").mockResolvedValue({ userDeletionGraceDays: 7 } as AdminSettings);
    const applyBatch = vi.spyOn(api, "applyAdminUserPermissionPresetBatch");
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(false);
    renderPage();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("爱丽丝"));
      container.querySelector<HTMLInputElement>('input[aria-label="选择爱丽丝"]')?.click();
      await vi.waitFor(() => expect(container.querySelector('option[value="preset-basic"]')).toBeTruthy());
      const select = container.querySelector<HTMLSelectElement>('select[aria-label="批量权限预设"]');
      if (select) {
        select.value = preset.id;
        select.dispatchEvent(new Event("change", { bubbles: true }));
      }
      container.querySelector<HTMLButtonElement>('button[aria-label="批量应用权限预设"]')?.click();
    });

    expect(confirm).toHaveBeenCalled();
    expect(applyBatch).not.toHaveBeenCalled();
  });

  it("clears selection when moving to another page", async () => {
    vi.spyOn(api, "adminUsers").mockImplementation(async ({ page } = {}) => ({
      users: [page === 2 ? user("user-b", "bob", "鲍勃") : user("user-a", "alice", "爱丽丝")],
      page: page ?? 1,
      pageSize: 20,
      totalCount: 21,
    }));
    vi.spyOn(api, "adminUserPermissionPresets").mockResolvedValue({ presets: [preset] });
    vi.spyOn(api, "adminSettings").mockResolvedValue({ userDeletionGraceDays: 7 } as AdminSettings);
    renderPage();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("爱丽丝"));
      container.querySelector<HTMLInputElement>('input[aria-label="选择爱丽丝"]')?.click();
      expect(container.textContent).toContain("当前页已选 1 人");
      container.querySelector<HTMLButtonElement>(".lux-admin-user-pagination button:last-child")?.click();
      await vi.waitFor(() => expect(container.textContent).toContain("鲍勃"));
    });

    expect(container.textContent).toContain("当前页已选 0 人");
    expect(container.querySelector<HTMLInputElement>('input[aria-label="选择鲍勃"]')?.checked).toBe(false);
  });

  function renderPage() {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => root.render(<QueryClientProvider client={queryClient}><MemoryRouter initialEntries={["/admin/users"]}><AdminUsersPage /></MemoryRouter></QueryClientProvider>));
  }
});
