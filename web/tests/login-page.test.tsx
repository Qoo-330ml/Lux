// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { LoginPage } from "../src/features/auth/LoginPage";
import { api } from "../src/lib/api/client";
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
    });

    expect(queryClient.getQueryData(queryKeys.me)).toEqual({ user });
  });
});
