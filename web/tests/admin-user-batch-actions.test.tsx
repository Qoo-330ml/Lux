// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AdminUsersPage } from "../src/features/admin/AdminUsersPage";
import { api } from "../src/lib/api/client";
import { chooseLuxOptionByValue, luxCombobox, luxOptions } from "./lux-select-helpers";
import type { AdminLibrary, AdminSettings, AdminUser, AdminUserPermissionPreset } from "../src/lib/api/types";

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

describe("AdminUsersPage batch user actions", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("confirms and disables only selected users on the current page, then shows per-user results", async () => {
    const alice = user("user-a", "alice", "爱丽丝");
    const bob = { ...user("user-b", "bob", "鲍勃"), isDisabled: true };
    const charlie = user("user-c", "charlie", "查理");
    const list = vi.spyOn(api, "adminUsers").mockResolvedValue({ users: [alice, bob, charlie], page: 1, pageSize: 20, totalCount: 4 });
    vi.spyOn(api, "adminUserPermissionPresets").mockResolvedValue({ presets: [preset] });
    vi.spyOn(api, "adminSettings").mockResolvedValue({ userDeletionGraceDays: 7 } as AdminSettings);
    const updateStatus = vi.spyOn(api, "updateAdminUserStatusBatch").mockResolvedValue({
      isDisabled: true,
      results: [
        { userId: alice.id, success: true, errorCode: null },
        { userId: bob.id, success: false, errorCode: "LAST_MANAGER" },
      ],
    });
    const confirm = vi.spyOn(window, "confirm").mockReturnValueOnce(false).mockReturnValue(true);
    renderPage();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("查理"));
      expect(container.querySelector('button[aria-label="批量禁用用户"]')).toBeNull();
      container.querySelector<HTMLInputElement>('input[aria-label="选择爱丽丝"]')?.click();
      container.querySelector<HTMLInputElement>('input[aria-label="选择鲍勃"]')?.click();
    });
    const disableButton = container.querySelector<HTMLButtonElement>('button[aria-label="批量禁用用户"]');
    expect(disableButton).toBeTruthy();
    expect(disableButton?.disabled).toBe(false);

    await act(async () => {
      disableButton?.click();
    });
    expect(updateStatus).not.toHaveBeenCalled();

    await act(async () => {
      disableButton?.click();
      await vi.waitFor(() => expect(updateStatus).toHaveBeenCalledWith([alice.id, bob.id], true));
      await vi.waitFor(() => expect(list).toHaveBeenCalledTimes(2));
    });
    expect(confirm).toHaveBeenLastCalledWith(expect.stringContaining("爱丽丝、鲍勃"));
    expect(confirm.mock.calls.at(-1)?.[0]).not.toContain("查理");
    expect(container.textContent).toContain("爱丽丝：成功");
    expect(container.textContent).toContain("鲍勃：最后一个管理员不能失去管理权限");
    expect(container.querySelector(".lux-users-selection")).toBeNull();
  });

  it("confirms enabling selected disabled users and reports that a new login is required", async () => {
    const disabledUser = { ...user("user-disabled", "disabled", "已禁用用户"), isDisabled: true };
    vi.spyOn(api, "adminUsers").mockResolvedValue({ users: [disabledUser], page: 1, pageSize: 20, totalCount: 1 });
    vi.spyOn(api, "adminUserPermissionPresets").mockResolvedValue({ presets: [preset] });
    vi.spyOn(api, "adminSettings").mockResolvedValue({ userDeletionGraceDays: 7 } as AdminSettings);
    const updateStatus = vi.spyOn(api, "updateAdminUserStatusBatch").mockResolvedValue({
      isDisabled: false,
      results: [{ userId: disabledUser.id, success: true, errorCode: null }],
    });
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    renderPage();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("已禁用用户"));
      container.querySelector<HTMLInputElement>('input[aria-label="选择已禁用用户"]')?.click();
      container.querySelector<HTMLButtonElement>('button[aria-label="批量启用用户"]')?.click();
      await vi.waitFor(() => expect(updateStatus).toHaveBeenCalledWith([disabledUser.id], false));
    });

    expect(confirm).toHaveBeenCalledWith(expect.stringContaining("需要重新登录"));
    expect(container.textContent).toContain("已禁用用户：成功");
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

    await act(async () => {
      container.querySelector<HTMLInputElement>('input[aria-label="选择爱丽丝"]')?.click();
      await vi.waitFor(() => expect(luxCombobox(container, "批量角色预设")?.hasAttribute("disabled")).toBe(false));
      container.querySelector<HTMLInputElement>('input[aria-label="选择鲍勃"]')?.click();
    });
    await chooseLuxOptionByValue(container, "批量角色预设", preset.id);
    expect(container.textContent).toContain("当前页已选 2 人");
    expect(container.textContent).toContain("仅对当前页勾选的用户操作");

    await act(async () => {
      container.querySelector<HTMLButtonElement>('button[aria-label="批量应用角色预设"]')?.click();
      await vi.waitFor(() => expect(applyBatch).toHaveBeenCalledWith([alice.id, bob.id], preset.id));
      await vi.waitFor(() => expect(list).toHaveBeenCalledTimes(2));
    });
    expect(confirm).toHaveBeenCalledWith(expect.stringContaining("爱丽丝、鲍勃"));
    expect(confirm.mock.calls[0]?.[0]).not.toContain("查理");
    expect(container.textContent).toContain("爱丽丝：成功");
    expect(container.textContent).toContain("鲍勃：最后一个管理员不能失去管理权限");
    expect(container.textContent).toContain("角色预设“普通用户”的应用结果");
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
      container.querySelector<HTMLInputElement>('input[aria-label="全选本页"]')?.click();
    });
    expect(container.textContent).toContain("当前页已选 2 人");
    expect([...container.querySelectorAll<HTMLInputElement>(".lux-admin-user-select input")].every((input) => input.checked)).toBe(true);

    await act(async () => {
      container.querySelector<HTMLInputElement>('input[aria-label="取消本页全选"]')?.click();
    });
    expect(container.querySelector(".lux-users-selection")).toBeNull();
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
      await vi.waitFor(() => expect(luxCombobox(container, "批量角色预设")?.hasAttribute("disabled")).toBe(false));
    });
    await chooseLuxOptionByValue(container, "批量角色预设", preset.id);
    await act(async () => {
      container.querySelector<HTMLButtonElement>('button[aria-label="批量应用角色预设"]')?.click();
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
      container.querySelector<HTMLButtonElement>('button[aria-label="下一页"]')?.click();
      await vi.waitFor(() => expect(container.textContent).toContain("鲍勃"));
    });

    expect(container.querySelector(".lux-users-selection")).toBeNull();
    expect(container.querySelector<HTMLInputElement>('input[aria-label="选择鲍勃"]')?.checked).toBe(false);
  });

  it("previews selected permission groups and applies them only to selected current-page users", async () => {
    const source = user("source", "source-user", "来源账号");
    const alice = user("user-a", "alice", "爱丽丝");
    const bob = user("user-b", "bob", "鲍勃");
    const unselected = user("user-c", "charlie", "未选择用户");
    const list = vi.spyOn(api, "adminUsers").mockResolvedValue({ users: [source, alice, bob, unselected], page: 1, pageSize: 20, totalCount: 4 });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [{ id: "library-a", name: "电影", kind: "MOVIE" } as AdminLibrary] });
    vi.spyOn(api, "adminUserPermissionPresets").mockResolvedValue({ presets: [preset] });
    vi.spyOn(api, "adminSettings").mockResolvedValue({ userDeletionGraceDays: 7 } as AdminSettings);
    const preview = vi.spyOn(api, "previewAdminUserPermissionCopy").mockResolvedValue({
      sourceUserId: source.id,
      sourceVersion: "a".repeat(64),
      sourceSnapshot: {
        library: { accessMode: "SELECTED", libraryIds: ["library-a"] },
        playback: { canDownload: false, canPlay: true, maxSimultaneousPlaybacks: 2, allowRemux: true, allowAudioTranscode: false, allowVideoTranscode: false },
      },
      targets: [
        { userId: alice.id, expectedVersion: "b".repeat(64) },
        { userId: bob.id, expectedVersion: "c".repeat(64) },
      ],
      groups: { library: true, content: false, playback: true },
    });
    const apply = vi.spyOn(api, "copyAdminUserPermissionGroups").mockResolvedValue({
      sourceUserId: source.id,
      sourceVersion: "a".repeat(64),
      results: [
        { userId: alice.id, success: true, errorCode: null },
        { userId: bob.id, success: false, errorCode: "VERSION_CONFLICT" },
      ],
    });
    renderPage();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("未选择用户"));
      container.querySelector<HTMLInputElement>('input[aria-label="选择爱丽丝"]')?.click();
      container.querySelector<HTMLInputElement>('input[aria-label="选择鲍勃"]')?.click();
      container.querySelector<HTMLButtonElement>('button[aria-label="复制权限"]')?.click();
      await vi.waitFor(() => expect(container.querySelector('[role="dialog"][aria-label="权限复制"]')).toBeTruthy());
    });

    expect(container.querySelector<HTMLButtonElement>('button[aria-label="批量应用角色预设"]')?.disabled).toBe(true);
    expect(container.textContent).toContain("目标账号 · 2 人");
    expect(container.textContent).toContain("爱丽丝");
    expect(container.textContent).toContain("鲍勃");
    const copySources = await luxOptions(container, "权限复制来源");
    expect(copySources.some((option) => option.value === "user-a")).toBe(false);
    expect(copySources.some((option) => option.value === "user-b")).toBe(false);
    expect(copySources.some((option) => option.value === "source")).toBe(true);
    await chooseLuxOptionByValue(container, "权限复制来源", source.id);

    await act(async () => {
      container.querySelector<HTMLInputElement>('input[aria-label="复制内容限制"]')?.click();
      container.querySelector<HTMLButtonElement>('button[aria-label="预览权限复制"]')?.click();
      await vi.waitFor(() => expect(preview).toHaveBeenCalledWith({
        sourceUserId: source.id,
        targetUserIds: [alice.id, bob.id],
        groups: { library: true, content: false, playback: true },
      }));
    });

    expect(container.textContent).toContain("最大同时播放 2 路");
    expect(container.textContent).toContain("指定：电影");
    expect(container.textContent).not.toContain("分级体系");
    await act(async () => {
      container.querySelector<HTMLButtonElement>('button[aria-label="应用权限复制"]')?.click();
      await vi.waitFor(() => expect(apply).toHaveBeenCalledWith({
        sourceUserId: source.id,
        sourceVersion: "a".repeat(64),
        targets: [
          { userId: alice.id, expectedVersion: "b".repeat(64) },
          { userId: bob.id, expectedVersion: "c".repeat(64) },
        ],
        groups: { library: true, content: false, playback: true },
      }));
      await vi.waitFor(() => expect(list.mock.calls.filter(([input]) => input?.pageSize === 20)).toHaveLength(2));
    });

    expect(container.textContent).toContain("爱丽丝：成功");
    expect(container.textContent).toContain("鲍勃：权限已变化，请重新预览");
    expect(container.querySelector(".lux-users-selection")).toBeNull();
    await act(async () => {
      container.querySelector<HTMLButtonElement>('button[aria-label="关闭权限复制"]')?.click();
      await vi.waitFor(() => expect(container.querySelector('[role="dialog"][aria-label="权限复制"]')).toBeNull());
    });
    await vi.waitFor(() => expect(document.activeElement).toBe(container.querySelector<HTMLHeadingElement>("#lux-users-list-title")));
  });

  it("invalidates a permission preview when the selected groups change", async () => {
    const source = user("source", "source-user", "来源账号");
    const target = user("user-a", "alice", "爱丽丝");
    vi.spyOn(api, "adminUsers").mockResolvedValue({ users: [source, target], page: 1, pageSize: 20, totalCount: 2 });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [] });
    vi.spyOn(api, "adminUserPermissionPresets").mockResolvedValue({ presets: [preset] });
    vi.spyOn(api, "adminSettings").mockResolvedValue({ userDeletionGraceDays: 7 } as AdminSettings);
    const preview = vi.spyOn(api, "previewAdminUserPermissionCopy").mockResolvedValue({
      sourceUserId: source.id,
      sourceVersion: "a".repeat(64),
      sourceSnapshot: { library: { accessMode: "ALL", libraryIds: [] } },
      targets: [{ userId: target.id, expectedVersion: "b".repeat(64) }],
      groups: { library: true, content: true, playback: true },
    });
    const apply = vi.spyOn(api, "copyAdminUserPermissionGroups");
    renderPage();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("爱丽丝"));
      container.querySelector<HTMLInputElement>('input[aria-label="选择爱丽丝"]')?.click();
      container.querySelector<HTMLButtonElement>('button[aria-label="复制权限"]')?.click();
      await vi.waitFor(() => expect(container.querySelector('[role="dialog"][aria-label="权限复制"]')).toBeTruthy());
    });
    await chooseLuxOptionByValue(container, "权限复制来源", source.id);
    await act(async () => {
      container.querySelector<HTMLButtonElement>('button[aria-label="预览权限复制"]')?.click();
      await vi.waitFor(() => expect(preview).toHaveBeenCalledTimes(1));
    });

    expect(container.querySelector<HTMLButtonElement>('button[aria-label="应用权限复制"]')?.disabled).toBe(false);
    await act(async () => {
      container.querySelector<HTMLInputElement>('input[aria-label="复制播放权限"]')?.click();
    });
    expect(container.querySelector('[aria-label="权限复制预览"]')).toBeNull();
    expect(container.querySelector<HTMLButtonElement>('button[aria-label="应用权限复制"]')).toBeNull();
    expect(apply).not.toHaveBeenCalled();
  });

  function renderPage() {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => root.render(<QueryClientProvider client={queryClient}><MemoryRouter initialEntries={["/admin/users"]}><AdminUsersPage /></MemoryRouter></QueryClientProvider>));
  }
});
