// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AdminUsersPage } from "../src/features/admin/AdminUsersPage";
import { api } from "../src/lib/api/client";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("AdminUsersPage library access scope", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("shows no checked libraries as the all-enabled-libraries default", async () => {
    mockPageData();
    renderPage();
    await expandPermissions();
    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector(".lux-admin-library-access")).toBeTruthy());
    });

    const checkboxes = [...container.querySelectorAll<HTMLInputElement>(".lux-admin-library-access input")];
    expect(checkboxes.map((checkbox) => checkbox.checked)).toEqual([false, false]);
    expect(container.textContent).toContain(
      "未勾选任何媒体库表示可访问全部已启用媒体库；勾选后仅可访问所选媒体库。",
    );
  });

  it("checks only libraries returned in the explicit selection", async () => {
    mockPageData(["library-2"]);
    renderPage();
    await expandPermissions();
    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector(".lux-admin-library-access")).toBeTruthy());
    });

    const checkboxes = [...container.querySelectorAll<HTMLInputElement>(".lux-admin-library-access input")];
    expect(checkboxes.map((checkbox) => checkbox.checked)).toEqual([false, true]);
  });

  it("does not show unchecked controls when the access list failed to load", async () => {
    mockPageData();
    vi.spyOn(api, "adminUserLibraryAccess").mockRejectedValue(new Error("网络错误"));
    renderPage();
    await expandPermissions();
    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector('[role="alert"]')).toBeTruthy());
    });

    expect(container.querySelector(".lux-admin-library-access")).toBeNull();
    expect(container.textContent).toContain("读取媒体库权限失败：网络错误");
  });

  function mockPageData(libraryIds: string[] = []) {
    vi.spyOn(api, "adminUsers").mockResolvedValue({ users: [{
      id: "user-1", usernameNormalized: "viewer", displayName: "观众", isDisabled: false,
      isAdmin: false, canManageServer: false, canRemoteAccess: false, canDownload: false,
    }] });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [
      { id: "library-1", name: "电影库", kind: "MOVIE" },
      { id: "library-2", name: "剧集库", kind: "SERIES" },
    ] });
    vi.spyOn(api, "adminUserLibraryAccess").mockResolvedValue({ libraryIds });
  }

  function renderPage() {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => root.render(<QueryClientProvider client={queryClient}><MemoryRouter initialEntries={["/admin/users"]}><AdminUsersPage /></MemoryRouter></QueryClientProvider>));
  }

  async function expandPermissions() {
    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("观众"));
    });
    const manageButton = [...container.querySelectorAll("button")]
      .find((button) => button.textContent?.includes("管理权限"));
    act(() => manageButton?.click());
    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector(".lux-admin-user-detail")).toBeTruthy());
    });
  }
});
