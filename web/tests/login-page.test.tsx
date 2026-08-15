// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/app/App";
import { LoginPage } from "../src/features/auth/LoginPage";
import { ApiError, api } from "../src/lib/api/client";
import { queryKeys } from "../src/lib/api/query-keys";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("LoginPage session state", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
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
