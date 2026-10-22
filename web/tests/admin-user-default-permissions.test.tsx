// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AdminUserSettingsPage } from "../src/features/admin/AdminUserManagementPage";
import { api } from "../src/lib/api/client";
import type { AdminSettings } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

type NewUserPermissions = {
  libraryAccessMode: "ALL" | "SELECTED" | "NONE";
  libraryIds: string[];
  canDownload: boolean;
  canPlay: boolean;
  maxSimultaneousPlaybacks: number;
  allowRemux: boolean;
  allowAudioTranscode: boolean;
  allowVideoTranscode: boolean;
};

describe("AdminUserSettingsPage new user permission defaults", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("loads the three-state defaults and lists only enabled libraries", async () => {
    renderPage(defaults({ libraryAccessMode: "SELECTED", libraryIds: ["library-1"] }));
    await waitForEditor();

    expect(findMode("SELECTED").checked).toBe(true);
    expect(libraryCheckboxes()).toHaveLength(1);
    expect(container.textContent).not.toContain("停用库");
    expect(findNumberInput().value).toBe("2");
  });

  it("saves a complete selected-library permission snapshot and updates cache", async () => {
    const queryClient = renderPage(defaults());
    const update = vi.spyOn(api, "updateAdminSettings").mockImplementation(async (input) => settings(
      input.newUserPermissions ?? defaults(),
    ));
    await waitForEditor();

    await act(async () => {
      findMode("SELECTED").click();
      libraryCheckboxes()[0]?.click();
      findInput("允许下载").click();
      findInput("允许视频转码").click();
      changeNumberInput("3");
    });

    await act(async () => {
      findButton("保存默认权限").click();
      await vi.waitFor(() => expect(update).toHaveBeenCalledWith({
        newUserPermissions: {
          ...defaults(),
          libraryAccessMode: "SELECTED",
          libraryIds: ["library-1"],
          canDownload: true,
          maxSimultaneousPlaybacks: 3,
          allowVideoTranscode: false,
        },
      }));
    });

    expect(container.querySelector('[role="status"]')?.textContent).toBe("新用户默认权限已保存。");
    expect(queryClient.getQueryData<AdminSettings>(["admin", "settings"])?.newUserPermissions?.libraryAccessMode).toBe("SELECTED");
  });

  it("normalizes an empty selected-library draft to NONE before saving", async () => {
    const update = vi.spyOn(api, "updateAdminSettings").mockImplementation(async (input) => settings(
      input.newUserPermissions ?? defaults(),
    ));
    renderPage(defaults({ libraryAccessMode: "SELECTED", libraryIds: ["library-1"] }));
    await waitForEditor();

    await act(async () => {
      libraryCheckboxes()[0]?.click();
      await vi.waitFor(() => expect(findButton("保存默认权限").disabled).toBe(false));
      expect(container.textContent).toContain("空选择保存后会设为无媒体库权限");
      findButton("保存默认权限").click();
      await vi.waitFor(() => expect(update).toHaveBeenCalledWith({
        newUserPermissions: { ...defaults(), libraryAccessMode: "NONE", libraryIds: [] },
      }));
    });
  });

  it("rejects a negative limit and keeps a failed draft editable", async () => {
    const update = vi.spyOn(api, "updateAdminSettings").mockRejectedValue(new Error("网络错误"));
    renderPage(defaults());
    await waitForEditor();

    await act(async () => changeNumberInput("-1"));
    expect(findButton("保存默认权限").disabled).toBe(true);
    await act(async () => changeNumberInput("4"));
    await act(async () => {
      findButton("保存默认权限").click();
      await vi.waitFor(() => expect(update).toHaveBeenCalled());
      await vi.waitFor(() => expect(container.querySelector('[role="alert"]')?.textContent).toContain("网络错误"));
    });
    expect(findNumberInput().value).toBe("4");
    expect(findButton("保存默认权限").disabled).toBe(false);
  });

  it("edits versioned presets while keeping built-in presets read-only", async () => {
    const adminPreset = permissionPreset({
      id: "builtin-admin",
      name: "管理员",
      isBuiltin: true,
      isAdmin: true,
      canManageServer: true,
    });
    const familyPreset = permissionPreset({ id: "preset-family", name: "Family", version: 3 });
    const load = vi.spyOn(api, "adminUserPermissionPresets").mockResolvedValue({ presets: [adminPreset, familyPreset] });
    const update = vi.spyOn(api, "updateAdminUserPermissionPreset").mockResolvedValue({
      preset: permissionPreset({ id: familyPreset.id, name: "Friends", version: 4 }),
    });
    vi.spyOn(api, "deleteAdminUserPermissionPreset").mockResolvedValue(undefined);
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    renderPage(defaults(), "/admin/users/settings?section=presets");
    await waitForPresets();

    expect(container.textContent).toContain("管理员");
    expect(container.textContent).toContain("Family");
    expect(findActionButton("编辑预设 管理员").disabled).toBe(true);
    expect(findActionButton("删除预设 管理员").disabled).toBe(true);

    await act(async () => findActionButton("编辑预设 Family").click());
    await act(async () => {
      changeTextInput("预设名称", "Friends");
      findInput("允许下载").click();
      changePresetNumberInput("4");
    });
    await act(async () => {
      findButton("保存预设").click();
      await vi.waitFor(() => expect(update).toHaveBeenCalledWith(familyPreset.id, 3, {
        name: "Friends",
        description: "A reusable permission snapshot",
        isAdmin: false,
        canManageServer: false,
        permissions: { ...defaults(), canDownload: true, maxSimultaneousPlaybacks: 4 },
      }));
    });
    expect(container.querySelector('[role="status"]')?.textContent).toContain("已更新");
    await vi.waitFor(() => expect(load).toHaveBeenCalledTimes(2));

    await act(async () => {
      findActionButton("删除预设 Family").click();
      await vi.waitFor(() => expect(confirm).toHaveBeenCalled());
      await vi.waitFor(() => expect(api.deleteAdminUserPermissionPreset).toHaveBeenCalledWith(familyPreset.id, 3));
    });
  });

  it("creates a preset and keeps the draft after a server validation error", async () => {
    vi.spyOn(api, "adminUserPermissionPresets").mockResolvedValue({ presets: [
      permissionPreset({ id: "builtin-admin", name: "管理员", isBuiltin: true, isAdmin: true, canManageServer: true }),
      permissionPreset({ id: "builtin-user", name: "普通用户", isBuiltin: true }),
    ] });
    vi.spyOn(api, "createAdminUserPermissionPreset")
      .mockRejectedValueOnce(new Error("名称已存在"))
      .mockResolvedValueOnce({ preset: permissionPreset({ id: "preset-new", name: "Friends" }) });
    renderPage(defaults(), "/admin/users/settings?section=presets");
    await waitForPresets();

    await act(async () => findButton("新建角色预设").click());
    await act(async () => changeTextInput("预设名称", "Friends"));
    await act(async () => {
      findButton("保存预设").click();
      await vi.waitFor(() => expect(container.querySelector('[role="alert"]')?.textContent).toContain("名称已存在"));
    });
    expect((container.querySelector('input[aria-label="预设名称"]') as HTMLInputElement).value).toBe("Friends");

    await act(async () => {
      findButton("保存预设").click();
      await vi.waitFor(() => expect(api.createAdminUserPermissionPreset).toHaveBeenLastCalledWith({
        name: "Friends",
        description: null,
        isAdmin: false,
        canManageServer: false,
        permissions: defaults(),
      }));
    });
    expect(container.querySelector('[role="status"]')?.textContent).toContain("已创建");
  });

  function renderPage(permissionDefaults: NewUserPermissions, initialEntry = "/admin/users/settings?section=defaults") {
    vi.spyOn(api, "adminSettings").mockResolvedValue(settings(permissionDefaults));
    if (!vi.isMockFunction(api.adminUserPermissionPresets)) {
      vi.spyOn(api, "adminUserPermissionPresets").mockResolvedValue({ presets: [] });
    }
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [
      { id: "library-1", name: "电影库", kind: "MOVIE", isEnabled: true, realtimeWatchEnabled: false, realtimeMetadataAutoMatchEnabled: false, roots: [] },
      { id: "library-disabled", name: "停用库", kind: "MOVIE", isEnabled: false, realtimeWatchEnabled: false, realtimeMetadataAutoMatchEnabled: false, roots: [] },
    ] });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => root.render(
      <QueryClientProvider client={queryClient}>
        <MemoryRouter initialEntries={[initialEntry]}>
          <AdminUserSettingsPage />
        </MemoryRouter>
      </QueryClientProvider>,
    ));
    return queryClient;
  }

  async function waitForEditor() {
    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector(".lux-admin-new-user-permissions")).toBeTruthy());
    });
  }

  async function waitForPresets() {
    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector(".lux-admin-user-presets")).toBeTruthy());
      await vi.waitFor(() => expect(container.textContent).toContain("角色预设"));
    });
  }

  function defaults(changes: Partial<NewUserPermissions> = {}): NewUserPermissions {
    return {
      libraryAccessMode: "NONE",
      libraryIds: [],
      canDownload: false,
      canPlay: true,
      maxSimultaneousPlaybacks: 2,
      allowRemux: true,
      allowAudioTranscode: true,
      allowVideoTranscode: true,
      ...changes,
    };
  }

  function settings(permissionDefaults: NewUserPermissions) {
    return { newUserPermissions: permissionDefaults } as unknown as AdminSettings;
  }

  function findMode(mode: "ALL" | "SELECTED" | "NONE") {
    const radio = container.querySelector<HTMLInputElement>(`input[type="radio"][value="${mode}"]`);
    expect(radio, `radio for ${mode}`).toBeTruthy();
    return radio!;
  }

  function libraryCheckboxes() {
    return [...container.querySelectorAll<HTMLInputElement>(".lux-admin-library-access input")];
  }

  function findInput(label: string) {
    const input = [...container.querySelectorAll<HTMLInputElement>('input[type="checkbox"]')]
      .find((item) => item.getAttribute("aria-label") === label);
    expect(input, `checkbox ${label}`).toBeTruthy();
    return input!;
  }

  function findNumberInput() {
    const input = container.querySelector<HTMLInputElement>('input[aria-label="新用户同时播放上限"]');
    expect(input).toBeTruthy();
    return input!;
  }

  function changeNumberInput(value: string) {
    const input = findNumberInput();
    act(() => {
      const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
      setter?.call(input, value);
      input.dispatchEvent(new Event("input", { bubbles: true }));
      input.dispatchEvent(new Event("change", { bubbles: true }));
    });
  }

  function changePresetNumberInput(value: string) {
    const input = container.querySelector<HTMLInputElement>('input[aria-label="预设同时播放上限"]');
    expect(input).toBeTruthy();
    act(() => {
      const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
      setter?.call(input, value);
      input?.dispatchEvent(new Event("input", { bubbles: true }));
      input?.dispatchEvent(new Event("change", { bubbles: true }));
    });
  }

  function changeTextInput(label: string, value: string) {
    const input = container.querySelector<HTMLInputElement>(`input[aria-label="${label}"]`);
    expect(input).toBeTruthy();
    act(() => {
      const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
      setter?.call(input, value);
      input?.dispatchEvent(new Event("input", { bubbles: true }));
      input?.dispatchEvent(new Event("change", { bubbles: true }));
    });
  }

  function permissionPreset(changes: Partial<{
    id: string;
    name: string;
    version: number;
    isBuiltin: boolean;
    isAdmin: boolean;
    canManageServer: boolean;
  }> = {}) {
    return {
      id: "preset-custom",
      name: "Family",
      description: "A reusable permission snapshot",
      version: 1,
      isBuiltin: false,
      isAdmin: false,
      canManageServer: false,
      permissions: defaults(),
      ...changes,
    };
  }

  function findButton(label: string) {
    const button = [...container.querySelectorAll<HTMLButtonElement>("button")].find((item) => item.textContent?.includes(label));
    expect(button, `button containing ${label}`).toBeTruthy();
    return button!;
  }

  function findActionButton(label: string) {
    const button = container.querySelector<HTMLButtonElement>(`button[aria-label="${label}"]`);
    expect(button, `button ${label}`).toBeTruthy();
    return button!;
  }
});
