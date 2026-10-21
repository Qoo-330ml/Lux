// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AdminUserDetailPage } from "../src/features/admin/AdminUserManagementPage";
import { api } from "../src/lib/api/client";
import type { AccountPreferences, AdminUser } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("AdminUserDetailPage account preferences", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("shows the saved preferences grouped by appearance, home, and playback", async () => {
    const preferences = makePreferences({
      theme: "light",
      accentColor: "ocean",
      showMediaLibraries: false,
      audioLanguage: "日语",
      subtitleLanguage: "English",
      autoPlayNextEpisode: false,
      libraryOrder: ["library-1", "library-2"],
      playedPercent: 88,
      version: 2,
      updatedAt: 1_800_000_000,
    });
    const getPreferences = vi.spyOn(api, "adminUserAccountPreferences").mockResolvedValue(preferences);
    vi.spyOn(api, "adminUser").mockResolvedValue({ user: makeUser() });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({
      libraries: [
        { id: "library-1", name: "电影" },
        { id: "library-2", name: "剧集" },
      ] as never,
    });
    renderPreferences();

    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector(".lux-admin-user-preferences-panel h2")?.textContent).toBe("账户偏好"));
    });

    const panel = container.querySelector<HTMLElement>(".lux-admin-user-preferences-panel");
    expect(getPreferences).toHaveBeenCalledWith("user-1");
    expect(panel?.textContent).toContain("外观");
    expect(panel?.textContent).toContain("浅色");
    expect(panel?.textContent).toContain("海蓝");
    expect(panel?.textContent).toContain("首页");
    expect(panel?.textContent).toContain("隐藏");
    expect(panel?.textContent).toContain("电影");
    expect(panel?.textContent).toContain("剧集");
    expect(panel?.textContent).toContain("播放");
    expect(panel?.textContent).toContain("日语");
    expect(panel?.textContent).toContain("English");
    expect(panel?.textContent).toContain("88%");
    expect(panel?.textContent).toContain("版本 2");
    expect(panel?.textContent).toContain("本页只读");
    expect(panel?.querySelectorAll("button, input, select, textarea")).toHaveLength(0);
  });

  it("shows version zero defaults without querying libraries", async () => {
    vi.spyOn(api, "adminUser").mockResolvedValue({ user: makeUser() });
    const getPreferences = vi.spyOn(api, "adminUserAccountPreferences").mockResolvedValue(makePreferences());
    const getLibraries = vi.spyOn(api, "adminLibraries");
    renderPreferences();

    await waitForPreferences();
    const panel = container.querySelector<HTMLElement>(".lux-admin-user-preferences-panel");
    expect(panel?.textContent).toContain("深色");
    expect(panel?.textContent).toContain("银灰");
    expect(panel?.textContent).toContain("95%");
    expect(panel?.textContent).toContain("未设置个人排序");
    expect(panel?.textContent).toContain("尚未保存过偏好");
    expect(getPreferences).toHaveBeenCalledWith("user-1");
    expect(getLibraries).not.toHaveBeenCalled();
  });

  it("allows retry after the read-only preference request fails", async () => {
    vi.spyOn(api, "adminUser").mockResolvedValue({ user: makeUser() });
    const getPreferences = vi.spyOn(api, "adminUserAccountPreferences")
      .mockRejectedValueOnce(new Error("网络错误"))
      .mockResolvedValue(makePreferences());
    renderPreferences();

    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector('[role="alert"]')?.textContent).toContain("网络错误"));
    });
    const retry = [...container.querySelectorAll<HTMLButtonElement>("button")]
      .find((button) => button.textContent?.includes("重试读取偏好"));
    expect(retry).toBeTruthy();
    await act(async () => {
      retry!.click();
      await vi.waitFor(() => expect(container.querySelector(".lux-admin-user-preferences-panel h2")?.textContent).toBe("账户偏好"));
    });
    expect(getPreferences).toHaveBeenCalledTimes(2);
  });

  it("keeps deleted account preferences read-only", async () => {
    vi.spyOn(api, "adminUser").mockResolvedValue({ user: makeUser({ deletedAt: 1_800_000_000 }) });
    vi.spyOn(api, "adminUserAccountPreferences").mockResolvedValue(makePreferences());
    renderPreferences();

    await waitForPreferences();
    const panel = container.querySelector<HTMLElement>(".lux-admin-user-preferences-panel");
    expect(panel?.textContent).toContain("回收站账号的偏好保持只读");
    expect(panel?.querySelectorAll("button, input, select, textarea")).toHaveLength(0);
  });

  function renderPreferences() {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => root.render(
      <QueryClientProvider client={queryClient}>
        <MemoryRouter initialEntries={["/admin/users/user-1?section=preferences"]}>
          <Routes>
            <Route path="/admin/users" element={<div />} />
            <Route path="/admin/users/:userId" element={<AdminUserDetailPage />} />
            <Route path="*" element={<div>NO ROUTE</div>} />
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>,
    ));
  }

  async function waitForPreferences() {
    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector(".lux-admin-user-preferences-panel h2")?.textContent).toBe("账户偏好"));
    });
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

function makePreferences(overrides: Partial<AccountPreferences> = {}): AccountPreferences {
  return {
    theme: "dark",
    accentColor: "silver",
    showMediaLibraries: true,
    showContinueWatching: true,
    audioLanguage: "原始音轨",
    subtitleLanguage: "简体中文",
    autoPlayNextEpisode: true,
    libraryOrder: [],
    playedPercent: 95,
    version: 0,
    updatedAt: null,
    ...overrides,
  };
}
