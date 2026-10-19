// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AdminUserDetailPage } from "../src/features/admin/AdminUserManagementPage";
import { api } from "../src/lib/api/client";
import type { AdminUser } from "../src/lib/api/types";

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
