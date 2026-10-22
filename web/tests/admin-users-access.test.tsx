// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AdminUserDetailPage } from "../src/features/admin/AdminUserManagementPage";
import { api } from "../src/lib/api/client";
import { queryKeys } from "../src/lib/api/query-keys";
import type { AdminUser, AdminUserPermissionPreset } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("AdminUserDetailPage library access scope", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("shows ALL explicitly instead of treating empty selections as ambiguous", async () => {
    mockAdminData("ALL", []);
    renderPermissions();
    await waitForAccessEditor();

    expect(findMode("ALL").checked).toBe(true);
    expect(container.textContent).toContain("可访问所有已启用媒体库，包括之后新增的媒体库。");
    expect(container.textContent).not.toContain("未勾选任何媒体库表示");
    expect(container.querySelectorAll(".lux-admin-library-access input")).toHaveLength(0);
  });

  it("edits selected libraries and saves an explicit mode with the selected IDs", async () => {
    mockAdminData("SELECTED", ["library-2", "library-disabled"]);
    const update = vi.spyOn(api, "updateAdminUserLibraryAccess").mockResolvedValue({
      userId: "user-1",
      libraryAccessMode: "SELECTED",
      libraryIds: ["library-1", "library-2"],
    });
    renderPermissions();
    await waitForAccessEditor();

    expect(findMode("SELECTED").checked).toBe(true);
    expect(libraryCheckboxes().map((checkbox) => checkbox.checked)).toEqual([false, true]);
    expect(container.textContent).toContain("已停用媒体库当前不可访问；保存指定范围时会移除：停用库。");

    await act(async () => {
      libraryCheckboxes()[0]?.click();
    });
    await act(async () => {
      findButton("保存媒体库权限").click();
      await vi.waitFor(() => expect(update).toHaveBeenCalledWith("user-1", {
        libraryAccessMode: "SELECTED",
        libraryIds: ["library-1", "library-2"],
      }));
    });
    expect(container.textContent).toContain("媒体库访问权限已保存。");
  });

  it("saves NONE directly and explains that no media library is accessible", async () => {
    mockAdminData("SELECTED", ["library-1"]);
    const update = vi.spyOn(api, "updateAdminUserLibraryAccess").mockResolvedValue({
      userId: "user-1",
      libraryAccessMode: "NONE",
      libraryIds: [],
    });
    renderPermissions();
    await waitForAccessEditor();

    await act(async () => {
      findMode("NONE").click();
    });
    expect(container.textContent).toContain("不能访问任何媒体库。");
    expect(container.querySelectorAll(".lux-admin-library-access input")).toHaveLength(0);
    await act(async () => {
      findButton("保存媒体库权限").click();
      await vi.waitFor(() => expect(update).toHaveBeenCalledWith("user-1", {
        libraryAccessMode: "NONE",
        libraryIds: [],
      }));
    });
  });

  it("shows a save error and keeps the user's draft", async () => {
    mockAdminData("ALL", []);
    const update = vi.spyOn(api, "updateAdminUserLibraryAccess").mockRejectedValue(new Error("网络错误"));
    renderPermissions();
    await waitForAccessEditor();

    await act(async () => {
      findMode("NONE").click();
    });
    await act(async () => {
      findButton("保存媒体库权限").click();
      await vi.waitFor(() => expect(update).toHaveBeenCalled());
      await vi.waitFor(() => expect(container.querySelector('[role="alert"]')?.textContent).toContain("网络错误"));
    });
    expect(findMode("NONE").checked).toBe(true);
  });

  it("keeps deleted users' library permissions read-only", async () => {
    mockAdminData("SELECTED", ["library-1"], { deletedAt: 1_800_000_000 });
    const access = vi.spyOn(api, "adminUserLibraryAccess");
    renderPermissions();
    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector(".lux-admin-user-permissions")).toBeTruthy());
    });

    expect(container.textContent).toContain("回收站中的账号权限为只读，恢复后可修改。");
    expect(container.querySelector("input[type=radio]")).toBeNull();
    expect(access).not.toHaveBeenCalled();
  });

  it("shows the saved preset snapshot and applies a selected preset with cache refresh", async () => {
    mockAdminData("ALL", [], {
      presetSource: "已删除的 Family 预设",
      presetSourceDetails: {
        id: "deleted-family",
        name: "Family",
        version: 3,
        appliedAt: 1_800_000_000,
      },
    });
    const loadUser = vi.spyOn(api, "adminUser");
    const apply = vi.spyOn(api, "applyAdminUserPermissionPreset").mockResolvedValue(undefined);
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    const queryClient = renderPermissions();
    queryClient.setQueryData(queryKeys.adminUsers, { users: [], total: 0 });
    queryClient.setQueryData(["admin", "user-access", "user-1"], { userId: "user-1", libraryAccessMode: "ALL", libraryIds: [] });
    queryClient.setQueryData(["admin", "user", "user-1", "playback-policy"], { policy: {
      canPlay: true,
      maxSimultaneousPlaybacks: 2,
      allowRemux: true,
      allowAudioTranscode: true,
      allowVideoTranscode: true,
    } });
    await waitForAccessEditor();

    expect(container.textContent).toContain("Family");
    expect(container.textContent).toContain("版本 3");
    expect(container.querySelector("time[datetime='2027-01-15T08:00:00.000Z']")).toBeTruthy();

    await act(async () => {
      const select = container.querySelector<HTMLSelectElement>('select[aria-label="选择角色预设"]');
      expect(select).toBeTruthy();
      select!.value = "preset-family";
      select!.dispatchEvent(new Event("change", { bubbles: true }));
    });
    await act(async () => {
      findButton("应用所选预设").click();
      await vi.waitFor(() => expect(apply).toHaveBeenCalledWith("user-1", "preset-family"));
    });

    expect(confirm).toHaveBeenCalledWith(expect.stringContaining("替换该用户当前的角色、媒体库、下载、播放和转码权限"));
    expect(container.querySelector('[role="status"]')?.textContent).toBe("角色预设已应用，用户资料正在刷新。");
    expect(queryClient.getQueryState(queryKeys.adminUsers)?.isInvalidated).toBe(true);
    expect(vi.mocked(api.adminUserLibraryAccess)).toHaveBeenCalledTimes(2);
    expect(queryClient.getQueryState(["admin", "user", "user-1", "playback-policy"])?.isInvalidated).toBe(true);
    await vi.waitFor(() => expect(loadUser).toHaveBeenCalledTimes(2));
  });

  it("does not apply a preset when the administrator cancels confirmation", async () => {
    mockAdminData("ALL", []);
    const apply = vi.spyOn(api, "applyAdminUserPermissionPreset");
    vi.spyOn(window, "confirm").mockReturnValue(false);
    renderPermissions();
    await waitForAccessEditor();

    await act(async () => {
      const select = container.querySelector<HTMLSelectElement>('select[aria-label="选择角色预设"]');
      expect(select).toBeTruthy();
      select!.value = "preset-family";
      select!.dispatchEvent(new Event("change", { bubbles: true }));
    });
    await act(async () => findButton("应用所选预设").click());

    expect(apply).not.toHaveBeenCalled();
  });

  it("shows a preset loading error and disables applying until the list is available", async () => {
    mockAdminData("ALL", []);
    vi.spyOn(api, "adminUserPermissionPresets").mockRejectedValue(new Error("预设服务不可用"));
    renderPermissions();
    await waitForAccessEditor();
    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector('[role="alert"]')?.textContent).toContain("读取角色预设失败：预设服务不可用"));
    });

    expect(container.querySelector<HTMLSelectElement>('select[aria-label="选择角色预设"]')?.disabled).toBe(true);
    expect(findButton("应用所选预设").disabled).toBe(true);
  });

  it("shows an application error and keeps the preset selection available for retry", async () => {
    mockAdminData("ALL", []);
    const apply = vi.spyOn(api, "applyAdminUserPermissionPreset").mockRejectedValue(new Error("最后管理员不能降权"));
    vi.spyOn(window, "confirm").mockReturnValue(true);
    renderPermissions();
    await waitForAccessEditor();

    await act(async () => {
      const select = container.querySelector<HTMLSelectElement>('select[aria-label="选择角色预设"]');
      expect(select).toBeTruthy();
      select!.value = "preset-family";
      select!.dispatchEvent(new Event("change", { bubbles: true }));
    });
    await act(async () => {
      findButton("应用所选预设").click();
      await vi.waitFor(() => expect(container.querySelector('[role="alert"]')?.textContent).toContain("最后管理员不能降权"));
    });

    expect(apply).toHaveBeenCalledWith("user-1", "preset-family");
    expect(container.querySelector<HTMLSelectElement>('select[aria-label="选择角色预设"]')?.value).toBe("preset-family");
  });

  it("does not show default controls when the access list fails to load", async () => {
    mockAdminData("ALL", []);
    vi.spyOn(api, "adminUserLibraryAccess").mockRejectedValue(new Error("网络错误"));
    renderPermissions();
    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector('[role="alert"]')).toBeTruthy());
    });

    expect(container.querySelector(".lux-admin-user-permissions")).toBeNull();
    expect(container.textContent).toContain("读取媒体库权限失败：网络错误");
  });

  function mockAdminData(
    libraryAccessMode: "ALL" | "SELECTED" | "NONE",
    libraryIds: string[],
    userOverrides: Partial<AdminUser> = {},
  ) {
    vi.spyOn(api, "adminUser").mockResolvedValue({ user: makeUser(userOverrides) });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [
      { id: "library-1", name: "电影库", kind: "MOVIE", isEnabled: true },
      { id: "library-2", name: "剧集库", kind: "SERIES", isEnabled: true },
      { id: "library-disabled", name: "停用库", kind: "MOVIE", isEnabled: false },
    ] });
    vi.spyOn(api, "adminUserLibraryAccess").mockResolvedValue({ libraryAccessMode, libraryIds });
    vi.spyOn(api, "adminUserPermissionPresets").mockResolvedValue({ presets: [
      permissionPreset({ id: "builtin-user", name: "普通用户", isBuiltin: true }),
      permissionPreset({ id: "preset-family", name: "Family" }),
    ] });
  }

  function renderPermissions() {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => root.render(
      <QueryClientProvider client={queryClient}>
        <MemoryRouter initialEntries={["/admin/users/user-1?section=permissions"]}>
          <Routes>
            <Route path="/admin/users/:userId" element={<AdminUserDetailPage />} />
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>,
    ));
    return queryClient;
  }

  async function waitForAccessEditor() {
    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector(".lux-admin-user-permissions")).toBeTruthy());
    });
  }

  function findMode(mode: "ALL" | "SELECTED" | "NONE") {
    const radio = container.querySelector<HTMLInputElement>(`input[type="radio"][value="${mode}"]`);
    expect(radio, `radio for ${mode}`).toBeTruthy();
    return radio!;
  }

  function libraryCheckboxes() {
    return [...container.querySelectorAll<HTMLInputElement>(".lux-admin-library-access input")];
  }

  function findButton(label: string) {
    const button = [...container.querySelectorAll<HTMLButtonElement>("button")].find((item) => item.textContent?.includes(label));
    expect(button, `button containing ${label}`).toBeTruthy();
    return button!;
  }
});

function makeUser(overrides: Partial<AdminUser> = {}): AdminUser {
  return {
    id: "user-1",
    usernameNormalized: "viewer",
    displayName: "观众",
    isDisabled: false,
    isAdmin: false,
    ...overrides,
  };
}

function permissionPreset(overrides: Partial<AdminUserPermissionPreset>): AdminUserPermissionPreset {
  return {
    id: "preset-default",
    name: "Default",
    description: null,
    version: 1,
    isBuiltin: false,
    isAdmin: false,
    canManageServer: false,
    permissions: {
      libraryAccessMode: "NONE",
      libraryIds: [],
      canDownload: false,
      canPlay: true,
      maxSimultaneousPlaybacks: 2,
      allowRemux: true,
      allowAudioTranscode: true,
      allowVideoTranscode: true,
    },
    ...overrides,
  };
}
