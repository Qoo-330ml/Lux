// @vitest-environment jsdom

import { act } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AccountPage } from "../src/features/account/AccountPage";
import { LuxShell } from "../src/components/layout/LuxShell";
import { api } from "../src/lib/api/client";
import { accountSettingsStorageKey, applyAccountTheme, DEFAULT_ACCOUNT_SETTINGS, moveLibrary, readAccountSettings } from "../src/features/account/account-settings";
import { calculateAvatarCrop } from "../src/features/account/avatar-image";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const user = {
  id: "user-1",
  usernameNormalized: "owner",
  displayName: "影院主人",
};

function mockAvatarImageProcessing(width = 600, height = 900) {
  const bitmap = { width, height, close: vi.fn() };
  const drawImage = vi.fn();
  const arc = vi.fn();
  const fill = vi.fn();
  const beginPath = vi.fn();
  const context = { drawImage, arc, fill, beginPath, globalCompositeOperation: "source-over" };
  vi.stubGlobal("createImageBitmap", vi.fn().mockResolvedValue(bitmap));
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(context as unknown as CanvasRenderingContext2D);
  vi.spyOn(HTMLCanvasElement.prototype, "toBlob").mockImplementation((callback) => {
    callback(new Blob(["cropped avatar"], { type: "image/png" }));
  });
  return { drawImage, arc, context, bitmap };
}

describe("avatar crop geometry", () => {
  it("centers a square crop and moves it within the source image bounds", () => {
    expect(calculateAvatarCrop(400, 800, { zoom: 1, horizontal: 0, vertical: 0 })).toEqual({
      x: 0,
      y: 200,
      size: 400,
    });
    expect(calculateAvatarCrop(400, 800, { zoom: 2, horizontal: 1, vertical: -1 })).toEqual({
      x: 200,
      y: 0,
      size: 200,
    });
  });
});

describe("account settings", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    localStorage.clear();
    document.documentElement.removeAttribute("data-lux-theme");
    document.documentElement.removeAttribute("data-lux-accent");
    vi.spyOn(api, "libraries").mockResolvedValue({
      libraries: [
        { id: "movies", name: "电影", kind: "MOVIE" },
        { id: "series", name: "剧集", kind: "SERIES" },
      ],
    });
    vi.spyOn(api, "userSettings").mockResolvedValue({ playedPercent: 95 });
    vi.spyOn(api, "libraryOrder").mockResolvedValue({ libraryOrder: [] });
    vi.spyOn(api, "updateLibraryOrder").mockImplementation(async (input) => input);
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it("provides safe defaults and keeps the library order within its list", () => {
    expect(readAccountSettings()).toEqual(DEFAULT_ACCOUNT_SETTINGS);
    expect(moveLibrary(["movies", "series"], 1, "up")).toEqual(["series", "movies"]);
    expect(moveLibrary(["movies", "series"], 0, "up")).toEqual(["movies", "series"]);
  });

  it("keeps local preferences isolated between accounts", () => {
    localStorage.setItem(accountSettingsStorageKey("user-a"), JSON.stringify({ theme: "light" }));

    expect(readAccountSettings("user-a").theme).toBe("light");
    expect(readAccountSettings("user-b").theme).toBe("dark");
  });

  it("defaults to silver when no valid accent preference is saved", () => {
    expect(readAccountSettings(user.id).accentColor).toBe("silver");
    localStorage.setItem(accountSettingsStorageKey(user.id), JSON.stringify({ accentColor: "unknown" }));
    expect(readAccountSettings(user.id).accentColor).toBe("silver");
  });

  it.each(["silver", "berry", "ocean", "amber", "mint"])("keeps the saved %s accent preference", (accentColor) => {
    localStorage.setItem(accountSettingsStorageKey(user.id), JSON.stringify({ accentColor }));
    expect(readAccountSettings(user.id).accentColor).toBe(accentColor);
  });

  it("switches the favicon to match the selected theme", () => {
    const favicon = document.createElement("link");
    favicon.rel = "icon";
    document.head.append(favicon);

    applyAccountTheme("light");
    expect(favicon.href).toBe("http://localhost:3000/favicon.svg");

    applyAccountTheme("dark");
    expect(favicon.href).toBe("http://localhost:3000/favicon-white.svg");
  });

  it("updates the browser theme color to match the selected theme", () => {
    const themeColor = document.createElement("meta");
    themeColor.name = "theme-color";
    document.head.append(themeColor);

    try {
      applyAccountTheme("light");
      expect(themeColor.content).toBe("#f4f3f1");

      applyAccountTheme("dark");
      expect(themeColor.content).toBe("#050506");
    } finally {
      themeColor.remove();
    }
  });

  it("persists the selected accent color for the current account", async () => {
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <AccountPage user={user} />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    await act(async () => {
      container.querySelector<HTMLButtonElement>('[aria-label="选择强调色 海蓝"]')?.click();
    });

    expect(document.documentElement.dataset.luxAccent).toBe("ocean");
    expect(JSON.parse(localStorage.getItem(accountSettingsStorageKey(user.id)) ?? "{}")).toMatchObject({ accentColor: "ocean" });

    const silverOption = container.querySelector<HTMLButtonElement>('[aria-label="选择强调色 银灰"]');
    expect(silverOption).not.toBeNull();
    await act(async () => { silverOption?.click(); });
    expect(silverOption?.getAttribute("aria-pressed")).toBe("true");
    expect(document.documentElement.dataset.luxAccent).toBe("silver");
    expect(JSON.parse(localStorage.getItem(accountSettingsStorageKey(user.id)) ?? "{}")).toMatchObject({ accentColor: "silver" });

    await act(async () => {
      const lightOption = Array.from(container.querySelectorAll<HTMLButtonElement>(".lux-theme-options button"))
        .find((button) => button.textContent === "浅色");
      lightOption?.click();
    });
    expect(document.documentElement.dataset.luxTheme).toBe("light");
    expect(readAccountSettings(user.id)).toMatchObject({ theme: "light", accentColor: "silver" });
  });

  it("renders the current account settings sections", async () => {
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <AccountPage user={user} />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(container.querySelector("h1")?.textContent).toBe("账户设置");
    expect(container.textContent).not.toContain("YOUR LUX PROFILE");
    expect(container.textContent).toContain("主题");
    expect(container.textContent).toContain("首页排版");
    expect(container.textContent).toContain("播放");
    expect(container.textContent).toContain("账户");
    expect(container.querySelector("#appearance .lux-setting-divider")).toBeNull();
    expect(container.querySelector('[aria-label="上移媒体库 剧集"]')).toBeTruthy();
  });

  it("submits the self-service password change and clears the form after success", async () => {
    const updatePassword = vi.spyOn(api, "updatePassword").mockResolvedValue();
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <AccountPage user={user} />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const currentPassword = container.querySelector<HTMLInputElement>('input[autocomplete="current-password"]');
    const newPassword = container.querySelectorAll<HTMLInputElement>('input[autocomplete="new-password"]');
    const setValue = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
    await act(async () => {
      setValue?.call(currentPassword, "old password");
      currentPassword?.dispatchEvent(new Event("input", { bubbles: true }));
      currentPassword?.dispatchEvent(new Event("change", { bubbles: true }));
      setValue?.call(newPassword[0], "new password");
      newPassword[0]?.dispatchEvent(new Event("input", { bubbles: true }));
      newPassword[0]?.dispatchEvent(new Event("change", { bubbles: true }));
      setValue?.call(newPassword[1], "new password");
      newPassword[1]?.dispatchEvent(new Event("input", { bubbles: true }));
      newPassword[1]?.dispatchEvent(new Event("change", { bubbles: true }));
    });

    await act(async () => {
      container.querySelector<HTMLButtonElement>(".lux-password-panel button[type='submit']")?.click();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(updatePassword).toHaveBeenCalledWith({
      currentPassword: "old password",
      newPassword: "new password",
    });
    expect(currentPassword?.value).toBe("");
    expect(newPassword[0]?.value).toBe("");
    expect(newPassword[1]?.value).toBe("");
    expect(container.textContent).toContain("密码已修改");
  });

  it("defaults to the administrator library order and lets a user turn it off", async () => {
    const update = vi.spyOn(api, "updateUserSettings").mockResolvedValue({
      playedPercent: 95,
      useAdminLibraryOrder: false,
      libraryOrderForced: false,
    });
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <AccountPage user={user} />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const toggle = container.querySelector<HTMLInputElement>("input[aria-label='按照管理员顺序排序']");
    expect(toggle?.checked).toBe(true);
    await act(async () => {
      toggle?.click();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(update).toHaveBeenCalledWith({ useAdminLibraryOrder: false });
  });

  it("locks the administrator order toggle when the server forces it", async () => {
    vi.mocked(api.userSettings).mockResolvedValueOnce({
      playedPercent: 95,
      useAdminLibraryOrder: true,
      libraryOrderForced: true,
    });
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <AccountPage user={user} />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const toggle = container.querySelector<HTMLInputElement>("input[aria-label='按照管理员顺序排序']");
    expect(toggle?.checked).toBe(true);
    expect(toggle?.disabled).toBe(true);
  });

  it("keeps logout beside the sidebar account identity without the device and permission card", async () => {
    const logout = vi.spyOn(api, "logout").mockResolvedValue();
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <AccountPage user={{ ...user, canManageServer: true }} />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const profileCard = container.querySelector(".lux-account-profile-card");
    expect(profileCard?.querySelector(".lux-account-logout-button")?.textContent).toContain("退出登录");
    expect(profileCard?.textContent).toContain(user.usernameNormalized);
    expect(container.querySelector(".lux-account-footer-card")).toBeNull();
    expect(container.textContent).not.toContain("当前设备");
    expect(container.textContent).not.toContain("账户权限");

    await act(async () => {
      profileCard?.querySelector<HTMLButtonElement>(".lux-account-logout-button")?.click();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(logout).toHaveBeenCalledOnce();
  });

  it("saves the personal automatic watched threshold", async () => {
    const update = vi.spyOn(api, "updateUserSettings").mockResolvedValue({ playedPercent: 82 });
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <AccountPage user={user} />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const input = container.querySelector<HTMLInputElement>('[aria-label="自动标记已看百分比"]');
    expect(input?.value).toBe("95");
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(input, "82");
      input?.dispatchEvent(new Event("input", { bubbles: true }));
      input?.dispatchEvent(new Event("change", { bubbles: true }));
    });
    await act(async () => {
      [...container.querySelectorAll<HTMLButtonElement>("button")].find((button) => button.textContent === "保存")?.click();
    });

    expect(update).toHaveBeenCalledWith({ playedPercent: 82 });
    expect(container.textContent).toContain("播放阈值已保存");
  });

  it("uploads an avatar to the server only after the user explicitly saves it", async () => {
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const { drawImage, arc, context } = mockAvatarImageProcessing();
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ avatarUrl: "/api/v1/auth/avatar" }), { status: 200 }),
    );

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/account"]}>
            <Routes>
              <Route element={<LuxShell user={user} />}>
                <Route path="/account" element={<AccountPage user={user} />} />
              </Route>
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const file = new File(["avatar"], "avatar.png", { type: "image/png" });
    const input = container.querySelector<HTMLInputElement>('input[type="file"]');
    expect(Array.from(container.querySelectorAll("button")).some((button) => button.textContent?.includes("保存头像"))).toBe(true);
    Object.defineProperty(input, "files", { configurable: true, value: [file] });

    await act(async () => {
      input?.dispatchEvent(new Event("change", { bubbles: true }));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    await vi.waitFor(() => {
      const button = Array.from(container.querySelectorAll<HTMLButtonElement>("button"))
        .find((candidate) => candidate.textContent?.includes("保存头像"));
      expect(button).toBeTruthy();
      expect(button?.disabled).toBe(false);
    });
    const zoom = container.querySelector<HTMLInputElement>('[aria-label="头像缩放"]');
    const verticalPosition = container.querySelector<HTMLInputElement>('[aria-label="头像垂直位置"]');
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(zoom, "2");
      zoom?.dispatchEvent(new Event("input", { bubbles: true }));
      zoom?.dispatchEvent(new Event("change", { bubbles: true }));
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(verticalPosition, "0.25");
      verticalPosition?.dispatchEvent(new Event("input", { bubbles: true }));
      verticalPosition?.dispatchEvent(new Event("change", { bubbles: true }));
    });
    const saveButton = Array.from(container.querySelectorAll<HTMLButtonElement>("button"))
      .find((button) => button.textContent?.includes("保存头像"));

    await act(async () => {
      saveButton?.click();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/v1/auth/avatar",
      expect.objectContaining({ method: "PUT", credentials: "same-origin" }),
    );
    expect(drawImage).toHaveBeenCalledWith(expect.anything(), 150, 375, 300, 300, 0, 0, 512, 512);
    expect(context.globalCompositeOperation).toBe("destination-in");
    expect(arc).toHaveBeenCalledWith(256, 256, 256, 0, Math.PI * 2);
    expect(context.fill).toHaveBeenCalledOnce();
    const uploadBody = fetchMock.mock.calls.find(([url]) => url === "/api/v1/auth/avatar")?.[1]?.body;
    expect(uploadBody).toBeInstanceOf(File);
    expect((uploadBody as File).type).toBe("image/png");
    expect((uploadBody as File).name).toBe("avatar.png");
    expect(container.querySelector<HTMLImageElement>(".lux-avatar img")?.getAttribute("src")).toMatch(
      /^\/api\/v1\/auth\/avatar\?v=\d+$/,
    );
    expect(localStorage.getItem("lux.account.avatar:user-1")).toBeNull();
    expect(container.textContent).toContain("头像已保存");
  });

  it("lets the user adjust the avatar crop before saving a non-square image", async () => {
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/account"]}>
            <Routes>
              <Route element={<LuxShell user={user} />}>
                <Route path="/account" element={<AccountPage user={user} />} />
              </Route>
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const file = new File(["portrait"], "portrait.png", { type: "image/png" });
    const input = container.querySelector<HTMLInputElement>('input[type="file"]');
    Object.defineProperty(input, "files", { configurable: true, value: [file] });

    await act(async () => {
      input?.dispatchEvent(new Event("change", { bubbles: true }));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(container.querySelector('[aria-label="头像裁切预览"]')).not.toBeNull();
    expect(container.querySelector<HTMLInputElement>('[aria-label="头像缩放"]')?.type).toBe("range");
    expect(container.querySelector<HTMLInputElement>('[aria-label="头像水平位置"]')?.type).toBe("range");
    expect(container.querySelector<HTMLInputElement>('[aria-label="头像垂直位置"]')?.type).toBe("range");
    expect(container.querySelector<HTMLButtonElement>('[aria-label="重置头像裁切"]')).not.toBeNull();
    expect(container.textContent).toContain("保存后会生成透明边缘的圆形头像。");
  });

  it("persists a changed theme and reorders libraries from an accessible control", async () => {
    vi.mocked(api.userSettings).mockResolvedValueOnce({ playedPercent: 95, useAdminLibraryOrder: false, libraryOrderForced: false });
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <AccountPage user={user} />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    await act(async () => {
      container.querySelector<HTMLButtonElement>('[aria-label="切换到浅色模式"]')?.click();
      container.querySelector<HTMLButtonElement>('[aria-label="上移媒体库 剧集"]')?.click();
    });

    expect(document.documentElement.dataset.luxTheme).toBe("light");
    expect(JSON.parse(localStorage.getItem(accountSettingsStorageKey(user.id)) ?? "{}")).toMatchObject({
      theme: "light",
      libraryOrder: ["series", "movies"],
    });
    expect(container.querySelector(".lux-account-library-row")?.textContent).toContain("剧集");
  });

  it("supports dragging one library row before another", async () => {
    vi.mocked(api.userSettings).mockResolvedValueOnce({ playedPercent: 95, useAdminLibraryOrder: false, libraryOrderForced: false });
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <AccountPage user={user} />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const rows = container.querySelectorAll<HTMLElement>(".lux-account-library-row");
    await act(async () => {
      rows[1]?.dispatchEvent(new Event("dragstart", { bubbles: true }));
    });
    await act(async () => {
      rows[0]?.dispatchEvent(new Event("dragover", { bubbles: true, cancelable: true }));
      rows[0]?.dispatchEvent(new Event("drop", { bubbles: true }));
    });

    expect(container.querySelector(".lux-account-library-row")?.textContent).toContain("剧集");
  });
});
