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

    act(() => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <LoginPage />
        </QueryClientProvider>,
      );
    });

    await act(async () => {
      await vi.waitFor(() => {
        expect(container.querySelectorAll(".lux-auth-poster-waterfall img")).toHaveLength(2);
        expect(container.querySelectorAll(".lux-auth-poster-waterfall-column")).toHaveLength(3);
      });
    });
    expect(container.querySelector<HTMLImageElement>(".lux-auth-poster-waterfall img")?.src)
      .toContain("/emby/Items/123/Images/Primary?tag=poster-one");
    expect(container.querySelector(".lux-auth-poster-wall")).toBeNull();
  });

  it("adds poster columns when the waterfall has room", async () => {
    class FakeResizeObserver {
      constructor(private readonly callback: unknown) {}

      observe() {
        (this.callback as (entries: Array<{ contentRect: { width: number } }>) => void)([
          { contentRect: { width: 900 } },
        ]);
      }

      disconnect() {}
    }
    vi.stubGlobal("ResizeObserver", FakeResizeObserver);
    Object.defineProperty(window, "ResizeObserver", {
      configurable: true,
      value: FakeResizeObserver,
      writable: true,
    });
    vi.mocked(api.loginBackground).mockResolvedValue({
      source: "RECENTLY_ADDED",
      images: Array.from({ length: 8 }, (_, index) => `/poster-${index + 1}.jpg`),
    });
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
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(container.querySelectorAll(".lux-auth-poster-waterfall-column")).toHaveLength(4);
    expect(container.querySelector<HTMLDivElement>(".lux-auth-poster-waterfall")?.style.getPropertyValue(
      "--lux-auth-poster-column-count",
    )).toBe("4");
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

    await vi.waitFor(() => {
      expect(container.querySelector<HTMLImageElement>(".lux-auth-poster-wall")?.getAttribute("src"))
        .toBe("/lux-poster-wall.jpg");
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
