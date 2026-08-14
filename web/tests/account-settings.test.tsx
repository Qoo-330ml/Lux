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

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const user = {
  id: "user-1",
  usernameNormalized: "owner",
  displayName: "影院主人",
};

describe("account settings", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    localStorage.clear();
    document.documentElement.removeAttribute("data-lux-theme");
    vi.spyOn(api, "libraries").mockResolvedValue({
      libraries: [
        { id: "movies", name: "电影", kind: "MOVIE" },
        { id: "series", name: "剧集", kind: "SERIES" },
      ],
    });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
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

  it("switches the favicon to match the selected theme", () => {
    const favicon = document.createElement("link");
    favicon.rel = "icon";
    document.head.append(favicon);

    applyAccountTheme("light");
    expect(favicon.href).toBe("http://localhost:3000/favicon.svg");

    applyAccountTheme("dark");
    expect(favicon.href).toBe("http://localhost:3000/favicon-white.svg");
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

  it("uploads an avatar to the server only after the user explicitly saves it", async () => {
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
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
    expect(container.querySelector<HTMLImageElement>(".lux-avatar img")?.getAttribute("src")).toMatch(
      /^\/api\/v1\/auth\/avatar\?v=\d+$/,
    );
    expect(localStorage.getItem("lux.account.avatar:user-1")).toBeNull();
    expect(container.textContent).toContain("头像已保存");
  });

  it("persists a changed theme and reorders libraries from an accessible control", async () => {
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
