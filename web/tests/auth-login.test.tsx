// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/app/App";
import { ApiError, api } from "../src/lib/api/client";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

describe("login session handoff", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("refetches the full session before rendering the authenticated shell", async () => {
    const user = { id: "user-1", usernameNormalized: "owner", displayName: "影院主人" };
    vi.spyOn(api, "setupStatus").mockResolvedValue({ initialized: true });
    vi.spyOn(api, "me")
      .mockRejectedValueOnce(new ApiError("未登录", { status: 401 }))
      .mockResolvedValue({ user, serverName: "客厅 Lux" });
    vi.spyOn(api, "login").mockResolvedValue(user);
    vi.spyOn(api, "home").mockResolvedValue({ libraries: [], recommended: [], continueWatching: [] });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <App />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });

    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector(".lux-auth-form")).not.toBeNull());
    });
    const inputs = container.querySelectorAll<HTMLInputElement>(".lux-auth-form input");
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(inputs[0], "owner");
      inputs[0].dispatchEvent(new Event("input", { bubbles: true }));
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(inputs[1], "password");
      inputs[1].dispatchEvent(new Event("input", { bubbles: true }));
      container.querySelector<HTMLFormElement>(".lux-auth-form")?.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
    });

    await act(async () => {
      await vi.waitFor(() => expect(api.me).toHaveBeenCalledTimes(2));
      await vi.waitFor(() => expect(container.querySelector(".lux-header")).not.toBeNull());
    });
    expect(container.querySelector(".lux-auth-form")).toBeNull();
  });
});
