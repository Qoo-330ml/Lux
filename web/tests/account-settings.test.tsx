// @vitest-environment jsdom

import { act } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AccountPage } from "../src/features/account/AccountPage";
import { api } from "../src/lib/api/client";
import { accountSettingsStorageKey, DEFAULT_ACCOUNT_SETTINGS, moveLibrary, readAccountSettings } from "../src/features/account/account-settings";

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
