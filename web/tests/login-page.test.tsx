// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/app/App";
import { LoginPage } from "../src/features/auth/LoginPage";
import { ApiError, api } from "../src/lib/api/client";
import { queryKeys } from "../src/lib/api/query-keys";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("LoginPage session state", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    vi.spyOn(api, "loginBackground").mockResolvedValue({ source: "STATIC", images: [] });
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it("renders real recently added posters when the server provides them", async () => {
    vi.mocked(api.loginBackground).mockResolvedValue({
      source: "RECENTLY_ADDED",
      images: [
        "/emby/Items/123/Images/Primary?tag=poster-one",
        "/emby/Items/456/Images/Primary?tag=poster-two",
      ],
    });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <LoginPage />
        </QueryClientProvider>,
      );
    });

    await act(async () => {
      await vi.waitFor(() => {
        expect(container.querySelectorAll(".lux-auth-poster-waterfall img")).toHaveLength(2);
        expect(container.querySelectorAll(".lux-auth-poster-waterfall-column")).toHaveLength(5);
      });
    });
    expect(container.querySelector<HTMLImageElement>(".lux-auth-poster-waterfall img")?.src)
      .toContain("/emby/Items/123/Images/Primary?tag=poster-one");
    expect(container.querySelector(".lux-auth-poster-wall")).toBeNull();
  });

  it("keeps all five poster columns rendered regardless of available width", async () => {
    vi.mocked(api.loginBackground).mockResolvedValue({
      source: "RECENTLY_ADDED",
      images: Array.from({ length: 8 }, (_, index) => `/poster-${index + 1}.jpg`),
    });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <LoginPage />
        </QueryClientProvider>,
      );
    });

    await act(async () => {
      await vi.waitFor(() => {
        expect(container.querySelectorAll(".lux-auth-poster-waterfall-column")).toHaveLength(5);
      });
    });
    expect(container.querySelectorAll(".lux-auth-poster-waterfall-column")).toHaveLength(5);
    expect(container.querySelector<HTMLDivElement>(".lux-auth-poster-waterfall")?.style.getPropertyValue(
      "--lux-auth-poster-column-width",
    )).toBe("26%");
  });

  it("renders plugin poster feeds inside the existing waterfall and exposes TMDb credits", async () => {
    vi.mocked(api.loginBackground).mockResolvedValue({
      source: "PLUGIN:org.lux.tmdb-trending-background",
      contentKind: "POSTER_FEED",
      sourceName: "TMDb 日榜横幅",
      copyrightNotice: "TMDb 图片来源",
      items: [
        { imageUrl: "https://image.tmdb.org/t/p/w500/one.jpg", title: "电影一" },
        { imageUrl: "https://image.tmdb.org/t/p/w500/two.jpg", title: "剧集二" },
      ],
    });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <LoginPage />
        </QueryClientProvider>,
      );
    });

    await act(async () => {
      await vi.waitFor(() => {
        expect(container.querySelectorAll(".lux-auth-poster-waterfall img")).toHaveLength(2);
      });
    });
    expect(container.querySelectorAll(".lux-auth-poster-waterfall-column")).toHaveLength(5);
    expect(container.querySelector<HTMLImageElement>(".lux-auth-poster-waterfall img")?.src)
      .toContain("https://image.tmdb.org/t/p/w500/one.jpg");
    expect(container.querySelector<HTMLDetailsElement>(".lux-auth-credits")?.textContent)
      .toContain("TMDB");
    expect(container.querySelector<HTMLImageElement>(".lux-auth-tmdb-mark")?.src)
      .toBe("https://www.themoviedb.org/assets/2/v4/logos/v2/blue_long_2-9665a76b1ae401a510ec1e0ca40ddcb3b0cfe45f1d51b77a308fea0845885648.svg");
    expect(container.textContent).not.toContain("TMDb 图片来源");
  });

  it("renders a single plugin poster intact without using the feed or hero layout", async () => {
    vi.mocked(api.loginBackground).mockResolvedValue({
      source: "PLUGIN:org.lux.tmdb-trending-background",
      contentKind: "SINGLE_POSTER",
      sourceName: "TMDb 日榜横幅",
      items: [{ imageUrl: "https://image.tmdb.org/t/p/w500/trending.jpg", title: "日榜电影" }],
    });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <LoginPage />
        </QueryClientProvider>,
      );
    });

    await act(async () => {
      await vi.waitFor(() => {
        expect(container.querySelector<HTMLImageElement>(".lux-auth-single-poster")?.src)
          .toContain("https://image.tmdb.org/t/p/w500/trending.jpg");
      });
    });
    expect(container.querySelector(".lux-auth-poster-waterfall")).toBeNull();
    expect(container.querySelector(".lux-auth-hero-image")).toBeNull();
    expect(container.querySelector(".lux-auth-visual-fade")).toBeNull();
    expect(container.querySelector<HTMLDetailsElement>(".lux-auth-credits")?.textContent)
      .toContain("TMDB");

    await act(async () => {
      container.querySelector<HTMLImageElement>(".lux-auth-single-poster")
        ?.dispatchEvent(new Event("error"));
    });
    expect(container.querySelector<HTMLImageElement>(".lux-auth-poster-wall")?.src)
      .toContain("/lux-poster-wall.jpg");
    expect(container.querySelector(".lux-auth-single-poster")).toBeNull();
  });

  it("renders an original single image with clickable work and license attribution", async () => {
    vi.mocked(api.loginBackground).mockResolvedValue({
      source: "PLUGIN:org.lux.wikimedia-potd-background",
      contentKind: "SINGLE_IMAGE",
      sourceName: "Wikimedia Commons · Picture of the Day",
      items: [{
        imageUrl: "https://thumb.wikimedia.org/potd.jpg",
        title: "Violet-backed starling",
        copyrightNotice: "By Charles J. Sharp · CC BY-SA 4.0",
        attributionUrl: "https://commons.wikimedia.org/wiki/File:Potd.jpg",
        licenseUrl: "https://creativecommons.org/licenses/by-sa/4.0/",
      }],
    });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <LoginPage />
        </QueryClientProvider>,
      );
    });

    await act(async () => {
      await vi.waitFor(() => {
        expect(container.querySelector<HTMLImageElement>(".lux-auth-single-image")?.src)
          .toContain("https://thumb.wikimedia.org/potd.jpg");
      });
    });
    expect(container.querySelector(".lux-auth-single-image-content")
      ?.contains(container.querySelector(".lux-auth-single-image"))).toBe(true);
    expect(container.querySelector(".lux-auth-single-image-content")
      ?.contains(container.querySelector(".lux-auth-background-credit"))).toBe(true);
    const links = container.querySelectorAll<HTMLAnchorElement>(".lux-auth-background-credit a");
    expect([...links].map((link) => link.href)).toEqual([
      "https://commons.wikimedia.org/wiki/File:Potd.jpg",
      "https://creativecommons.org/licenses/by-sa/4.0/",
    ]);
    expect([...links].every((link) => link.target === "_blank" && link.rel.includes("noreferrer")))
      .toBe(true);
    expect(container.querySelector(".lux-auth-visual-fade")).toBeNull();
    expect(container.querySelector(".lux-auth-poster-waterfall")).toBeNull();
    expect(container.querySelector(".lux-auth-hero-image")).toBeNull();

    await act(async () => {
      container.querySelector<HTMLImageElement>(".lux-auth-single-image")
        ?.dispatchEvent(new Event("error"));
    });
    expect(container.querySelector<HTMLImageElement>(".lux-auth-poster-wall")?.src)
      .toContain("/lux-poster-wall.jpg");
    expect(container.querySelector(".lux-auth-single-image-content")).toBeNull();
  });

  it("renders plugin hero images with source attribution and falls back after an image error", async () => {
    vi.mocked(api.loginBackground).mockResolvedValue({
      source: "PLUGIN:org.lux.bing-daily-background",
      contentKind: "HERO_IMAGE",
      sourceName: "Bing 每日图片",
      copyrightNotice: "摄影者与版权信息",
      items: [{ imageUrl: "https://images.example.com/today.jpg", title: "今日主题" }],
    });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <LoginPage />
        </QueryClientProvider>,
      );
    });

    await act(async () => {
      await vi.waitFor(() => {
        expect(container.querySelector<HTMLImageElement>(".lux-auth-hero-image")?.src)
          .toContain("https://images.example.com/today.jpg");
      });
    });
    expect(container.textContent).toContain("摄影者与版权信息");
    expect(container.querySelector(".lux-auth-poster-waterfall")).toBeNull();

    await act(async () => {
      container.querySelector<HTMLImageElement>(".lux-auth-hero-image")
        ?.dispatchEvent(new Event("error"));
    });
    expect(container.querySelector<HTMLImageElement>(".lux-auth-poster-wall")?.getAttribute("src"))
      .toBe("/lux-poster-wall.jpg");
    expect(container.querySelector(".lux-auth-hero-image")).toBeNull();
  });

  it("falls back to the fixed wall when a plugin poster fails to load", async () => {
    vi.mocked(api.loginBackground).mockResolvedValue({
      source: "PLUGIN:org.lux.tmdb-trending-background",
      contentKind: "POSTER_FEED",
      sourceName: "TMDb 日榜横幅",
      items: [{ imageUrl: "https://image.tmdb.org/t/p/w500/broken.jpg", title: "失效海报" }],
    });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <LoginPage />
        </QueryClientProvider>,
      );
    });
    let poster: HTMLImageElement | null = null;
    await act(async () => {
      await vi.waitFor(() => {
        poster = container.querySelector<HTMLImageElement>(".lux-auth-poster-waterfall img");
        expect(poster?.src).toContain("https://image.tmdb.org/t/p/w500/broken.jpg");
      });
    });

    await act(async () => {
      poster?.dispatchEvent(new Event("error"));
    });
    expect(container.querySelector<HTMLImageElement>(".lux-auth-poster-wall")?.getAttribute("src"))
      .toBe("/lux-poster-wall.jpg");
    expect(container.querySelector(".lux-auth-poster-waterfall")).toBeNull();
  });

  it("falls back to the fixed wall when a plugin feed is empty", async () => {
    vi.mocked(api.loginBackground).mockResolvedValue({
      source: "PLUGIN:org.lux.tmdb-trending-background",
      contentKind: "POSTER_FEED",
      sourceName: "TMDb 日榜横幅",
      items: [],
    });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <LoginPage />
        </QueryClientProvider>,
      );
    });

    await act(async () => {
      await vi.waitFor(() => {
        expect(container.querySelector<HTMLImageElement>(".lux-auth-poster-wall")?.getAttribute("src"))
          .toBe("/lux-poster-wall.jpg");
      });
    });
  });

  it("keeps the fixed poster wall when the background request fails", async () => {
    vi.mocked(api.loginBackground).mockRejectedValue(new Error("background unavailable"));
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    act(() => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <LoginPage />
        </QueryClientProvider>,
      );
    });

    await act(async () => {
      await vi.waitFor(() => {
        expect(container.querySelector<HTMLImageElement>(".lux-auth-poster-wall")?.getAttribute("src"))
          .toBe("/lux-poster-wall.jpg");
      });
    });
    expect(container.querySelector(".lux-auth-poster-waterfall")).toBeNull();
  });

  it("renders standard credential autofill metadata", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    act(() => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <LoginPage />
        </QueryClientProvider>,
      );
    });

    const form = container.querySelector<HTMLFormElement>("form.lux-auth-form");
    const username = container.querySelector<HTMLInputElement>("#username");
    const password = container.querySelector<HTMLInputElement>("#password");

    expect(form?.getAttribute("autocomplete")).toBe("on");
    expect(username?.getAttribute("name")).toBe("username");
    expect(username?.getAttribute("autocomplete")).toBe("username");
    expect(password?.getAttribute("name")).toBe("password");
    expect(password?.getAttribute("autocomplete")).toBe("current-password");
  });

  it("stores the authenticated user under the session shape used by routes", async () => {
    const user = {
      id: "user-1",
      usernameNormalized: "admin",
      canManageServer: true,
    };
    vi.spyOn(api, "login").mockResolvedValue(user);
    vi.spyOn(api, "me").mockResolvedValue({ user, serverName: "客厅 Lux" });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    act(() => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <LoginPage />
        </QueryClientProvider>,
      );
    });

    const username = container.querySelector<HTMLInputElement>("input[autocomplete='username']");
    const password = container.querySelector<HTMLInputElement>("input[autocomplete='current-password']");
    const setInputValue = (input: HTMLInputElement, value: string) => {
      const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
      setter?.call(input, value);
      input.dispatchEvent(new Event("input", { bubbles: true }));
    };

    act(() => {
      setInputValue(username!, "admin");
      setInputValue(password!, "password");
    });
    await act(async () => {
      container.querySelector<HTMLButtonElement>("button[type='submit']")?.click();
      await vi.waitFor(() => expect(api.login).toHaveBeenCalledWith("admin", "password"));
      await vi.waitFor(() => expect(api.me).toHaveBeenCalledTimes(1));
    });

    expect(queryClient.getQueryData(queryKeys.me)).toEqual({ user, serverName: "客厅 Lux" });
  });

  it("renders the authenticated home page immediately after login", async () => {
    const user = {
      id: "user-1",
      usernameNormalized: "admin",
      canManageServer: true,
    };
    vi.spyOn(api, "setupStatus").mockResolvedValue({ initialized: true });
    let meCalls = 0;
    vi.spyOn(api, "me").mockImplementation(async () => {
      meCalls += 1;
      if (meCalls === 1) throw new ApiError("需要登录", { status: 401 });
      return { user, serverName: "客厅 Lux" };
    });
    vi.spyOn(api, "login").mockResolvedValue(user);
    vi.spyOn(api, "home").mockResolvedValue({ libraries: [], recommended: [], continueWatching: [] });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    act(() => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/login"]}>
            <App />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    await vi.waitFor(() => expect(container.querySelector("input[autocomplete='username']")).not.toBeNull());
    const username = container.querySelector<HTMLInputElement>("input[autocomplete='username']");
    const password = container.querySelector<HTMLInputElement>("input[autocomplete='current-password']");
    const setInputValue = (input: HTMLInputElement, value: string) => {
      const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
      setter?.call(input, value);
      input.dispatchEvent(new Event("input", { bubbles: true }));
    };

    act(() => {
      setInputValue(username!, "admin");
      setInputValue(password!, "password");
    });
    await act(async () => {
      container.querySelector<HTMLButtonElement>("button[type='submit']")?.click();
      await vi.waitFor(() => expect(api.login).toHaveBeenCalledWith("admin", "password"));
    });

    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector(".lux-home")).not.toBeNull());
    });
    expect(api.me).toHaveBeenCalledTimes(2);
    expect(queryClient.getQueryData(queryKeys.me)).toEqual({ user, serverName: "客厅 Lux" });
  });
});
