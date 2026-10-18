// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AdminUserDetailPage } from "../src/features/admin/AdminUserManagementPage";
import { api } from "../src/lib/api/client";
import type { AdminUser } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("AdminUserDetailPage account lifecycle", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("shows the normal status and confirms before disabling the account", async () => {
    const queryClient = renderDetail(makeUser());
    const invalidate = vi.spyOn(queryClient, "invalidateQueries");
    const disable = vi.spyOn(api, "disableAdminUser").mockResolvedValue({ user: makeUser({ isDisabled: true }) });
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(false);
    await waitForUser();

    expect(container.textContent).toContain("正常");
    const button = findButton("禁用账号");
    act(() => button.click());
    expect(confirm).toHaveBeenCalledWith("禁用“观众”后会撤销登录凭证并停止播放，账号资料仍会保留。确定继续吗？");
    expect(disable).not.toHaveBeenCalled();

    confirm.mockReturnValue(true);
    await act(async () => {
      button.click();
      await vi.waitFor(() => expect(disable).toHaveBeenCalledWith("user-1"));
    });
    expect(queryClient.getQueryData(["admin", "user", "user-1"])).toEqual({ user: makeUser({ isDisabled: true }) });
    expect(invalidate).toHaveBeenCalledWith(expect.objectContaining({ queryKey: ["admin", "users"] }));
  });

  it("shows an enable action for disabled accounts", async () => {
    const user = makeUser({ isDisabled: true });
    renderDetail(user);
    const enable = vi.spyOn(api, "updateAdminUser").mockResolvedValue({ user: makeUser() });
    vi.spyOn(window, "confirm").mockReturnValue(true);
    await waitForUser();

    await act(async () => {
      findButton("启用账号").click();
      await vi.waitFor(() => expect(enable).toHaveBeenCalledWith("user-1", { isDisabled: false }));
    });
  });

  it("makes deleted account details read-only and restores back to the user list", async () => {
    const deletedUser = makeUser({ isDisabled: true, deletedAt: 1_800_000_000, deletionPurgeAt: 1_800_604_800 });
    const queryClient = renderDetail(deletedUser);
    const restore = vi.spyOn(api, "restoreAdminUser").mockResolvedValue({ user: makeUser() });
    await waitForUser();

    const profile = container.querySelector(".lux-admin-user-profile-form");
    expect(container.querySelector(".lux-admin-user-lifecycle-panel")?.textContent).toContain("回收站");
    expect(container.querySelector(".lux-admin-user-profile-panel .lux-user-badge")?.textContent).toBe("回收站");
    expect(profile?.textContent).toContain("回收站中的账号资料为只读");
    expect(profile?.querySelector<HTMLInputElement>('input[type="file"]')?.disabled).toBe(true);
    expect([...profile!.querySelectorAll<HTMLInputElement>("input")].every((input) => input.disabled)).toBe(true);
    expect(findButton("恢复账号")).toBeTruthy();
    expect(findButton("彻底删除")).toBeTruthy();

    await act(async () => {
      findButton("恢复账号").click();
      await vi.waitFor(() => expect(restore).toHaveBeenCalledWith("user-1"));
      await vi.waitFor(() => expect(queryClient.getQueryData<{ user: AdminUser }>(["admin", "user", "user-1"])?.user.deletedAt).toBeUndefined());
    });
    await vi.waitFor(() => expect(container.querySelector("[data-location]")?.textContent).toBe("/admin/users"));
  });

  it("confirms the permanent deletion impact before purging a deleted account", async () => {
    renderDetail(makeUser({ isDisabled: true, deletedAt: 1_800_000_000, deletionPurgeAt: 1_800_604_800 }));
    const purge = vi.spyOn(api, "purgeAdminUser").mockResolvedValue(undefined);
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(false);
    await waitForUser();

    act(() => findButton("彻底删除").click());
    expect(confirm).toHaveBeenCalledWith("彻底删除“观众”将永久删除账号、头像、权限设置、偏好、播放记录和设备关联，媒体文件不受影响，且无法恢复。确定继续吗？");
    expect(purge).not.toHaveBeenCalled();
  });

  function renderDetail(user: AdminUser) {
    vi.spyOn(api, "adminUser").mockResolvedValue({ user });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => root.render(
      <QueryClientProvider client={queryClient}>
        <MemoryRouter initialEntries={["/admin/users/user-1"]}>
          <LocationOutput />
          <Routes>
            <Route path="/admin/users" element={<div />} />
            <Route path="/admin/users/:userId" element={<AdminUserDetailPage />} />
            <Route path="*" element={<div />} />
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>,
    ));
    return queryClient;
  }

  async function waitForUser() {
    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector(".lux-admin-user-profile-panel")).toBeTruthy());
    });
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
    email: "viewer@example.com",
    ...overrides,
  };
}

function LocationOutput() {
  const location = useLocation();
  return <output data-location>{location.pathname}{location.search}</output>;
}
