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

  function renderPage(permissionDefaults: NewUserPermissions) {
    vi.spyOn(api, "adminSettings").mockResolvedValue(settings(permissionDefaults));
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
        <MemoryRouter initialEntries={["/admin/users/settings?section=defaults"]}>
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

  function findButton(label: string) {
    const button = [...container.querySelectorAll<HTMLButtonElement>("button")].find((item) => item.textContent?.includes(label));
    expect(button, `button containing ${label}`).toBeTruthy();
    return button!;
  }
});
