// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AdminUsersPage } from "../src/features/admin/AdminUsersPage";
import { api } from "../src/lib/api/client";
import type { AdminSettings } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("AdminUsersPage row entry points", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("opens user management from the avatar, the display name and the account id", async () => {
    vi.spyOn(api, "adminUsers").mockResolvedValue({ users: [{
      id: "user-1", usernameNormalized: "viewer", displayName: "观众", isDisabled: false,
      isAdmin: false, canManageServer: false, canRemoteAccess: false, canDownload: false,
    }] });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [] });
    renderPage();

    await act(async () => { await vi.waitFor(() => expect(container.textContent).toContain("观众")); });

    const row = container.querySelector(".lux-users-row");
    expect(row).toBeTruthy();
    const expected = "/admin/users/user-1";
    expect(row?.querySelector("a.lux-users-avatar")?.getAttribute("href")).toBe(expected);
    expect(row?.querySelector("a.lux-users-account-name")?.getAttribute("href")).toBe(expected);
    expect(row?.querySelector("a.lux-users-account-username")?.getAttribute("href")).toBe(expected);
    expect(row?.querySelector("a.lux-users-account-username")?.textContent).toContain("@viewer");
  });

  it("keeps the same three entry points on the compact user card", async () => {
    vi.spyOn(api, "adminUsers").mockResolvedValue({ users: [{
      id: "user-1", usernameNormalized: "viewer", displayName: "观众", isDisabled: false,
      isAdmin: false, canManageServer: false, canRemoteAccess: false, canDownload: false,
    }] });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [] });
    renderPage();

    await act(async () => { await vi.waitFor(() => expect(container.textContent).toContain("观众")); });

    const card = container.querySelector(".lux-users-card-item");
    expect(card).toBeTruthy();
    const expected = "/admin/users/user-1";
    expect(card?.querySelector("a.lux-users-avatar")?.getAttribute("href")).toBe(expected);
    expect(card?.querySelector("a.lux-users-account-name")?.getAttribute("href")).toBe(expected);
    expect(card?.querySelector("a.lux-users-account-username")?.getAttribute("href")).toBe(expected);
  });

  it("does not turn the avatar into a separate tab stop", async () => {
    vi.spyOn(api, "adminUsers").mockResolvedValue({ users: [{
      id: "user-1", usernameNormalized: "viewer", displayName: "观众", isDisabled: false,
      isAdmin: false, canManageServer: false, canRemoteAccess: false, canDownload: false,
    }] });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [] });
    renderPage();

    await act(async () => { await vi.waitFor(() => expect(container.textContent).toContain("观众")); });

    const row = container.querySelector(".lux-users-row");
    expect(row?.querySelector("a.lux-users-avatar")?.getAttribute("tabindex")).toBe("-1");
    expect(row?.querySelector("a.lux-users-account-username")?.getAttribute("tabindex")).toBe("-1");
    expect(row?.querySelector("a.lux-users-account-name")?.hasAttribute("tabindex")).toBe(false);
  });

  function renderPage() {
    vi.spyOn(api, "adminSettings").mockResolvedValue({ userDeletionGraceDays: 7 } as AdminSettings);
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => root.render(<QueryClientProvider client={queryClient}><MemoryRouter initialEntries={["/admin/users"]}><AdminUsersPage /></MemoryRouter></QueryClientProvider>));
  }
});
