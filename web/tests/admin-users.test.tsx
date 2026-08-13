// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AdminUsersPage } from "../src/features/admin/AdminUsersPage";
import { api } from "../src/lib/api/client";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("AdminUsersPage", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("lets an administrator edit a user's login name and display name together", async () => {
    vi.spyOn(api, "adminUsers").mockResolvedValue({ users: [{
      id: "user-1",
      usernameNormalized: "viewer",
      displayName: "观众",
      isDisabled: false,
      isAdmin: false,
      canManageServer: false,
      canRemoteAccess: false,
      canDownload: false,
    }] });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [] });
    vi.spyOn(api, "adminUserLibraryAccess").mockResolvedValue({ libraryIds: [] });
    const update = vi.spyOn(api, "updateAdminUser").mockResolvedValue({ user: {
      id: "user-1",
      usernameNormalized: "family",
      displayName: "家人",
      isDisabled: false,
      isAdmin: false,
    } });
    renderPage();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("观众"));
    });
    act(() => container.querySelector<HTMLButtonElement>('button[aria-label="管理观众账户"]')?.click());

    const username = container.querySelector<HTMLInputElement>('input[aria-label="观众的用户名"]');
    const displayName = container.querySelector<HTMLInputElement>('input[aria-label="观众的昵称"]');
    expect(username?.value).toBe("viewer");
    expect(displayName?.value).toBe("观众");
    act(() => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(username, "Family");
      username?.dispatchEvent(new Event("input", { bubbles: true }));
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(displayName, "家人");
      displayName?.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await act(async () => {
      container.querySelector<HTMLButtonElement>('button[aria-label="保存观众账户信息"]')?.click();
      await vi.waitFor(() => expect(update).toHaveBeenCalledWith("user-1", {
        username: "Family",
        displayName: "家人",
      }));
    });
  });

  function renderPage() {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => root.render(<QueryClientProvider client={queryClient}><AdminUsersPage /></QueryClientProvider>));
  }
});
