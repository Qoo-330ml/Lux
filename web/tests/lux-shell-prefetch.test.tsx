// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { LuxShell } from "../src/components/layout/LuxShell";
import { queryKeys } from "../src/lib/api/query-keys";

const routeModules = vi.hoisted(() => ({
  libraries: false,
  favorites: false,
  search: false,
  account: false,
  admin: false,
}));

vi.mock("../src/features/library/LibrariesPage", () => {
  routeModules.libraries = true;
  return { LibrariesPage: () => null };
});
vi.mock("../src/features/favorites/FavoritesPage", () => {
  routeModules.favorites = true;
  return { FavoritesPage: () => null };
});
vi.mock("../src/features/search/SearchPage", () => {
  routeModules.search = true;
  return { SearchPage: () => null };
});
vi.mock("../src/features/account/AccountPage", () => {
  routeModules.account = true;
  return { AccountPage: () => null };
});
vi.mock("../src/features/admin/AdminLayout", () => {
  routeModules.admin = true;
  return { AdminLayout: () => null };
});
// The console entry prefetches the dashboard page and its data. Mock the page so the assertion
// below can only be satisfied by the prefetch, not by the page rendering itself.
vi.mock("../src/features/admin/AdminDashboardPage", () => ({
  AdminDashboardPage: () => null,
}));
vi.mock("../src/features/activity/ScanActivityPopover", () => ({ ScanActivityPopover: () => null }));

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("LuxShell route preloading", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("preloads frequently used lazy routes on pointer hover or keyboard focus", async () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const prefetchQuery = vi.spyOn(queryClient, "prefetchQuery").mockResolvedValue(undefined);

    act(() => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/"]}>
            <Routes>
              <Route element={<LuxShell user={{ id: "user-1", usernameNormalized: "test", canManageServer: true }} />}>
                <Route index element={null} />
              </Route>
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });

    const preloadByPointer = async (selector: string, moduleName: keyof typeof routeModules) => {
      const link = container.querySelector<HTMLAnchorElement>(selector);
      expect(link).not.toBeNull();
      act(() => link?.dispatchEvent(new Event("pointerover", { bubbles: true })));
      await vi.waitFor(() => expect(routeModules[moduleName]).toBe(true));
    };
    const preloadByFocus = async (selector: string, moduleName: keyof typeof routeModules) => {
      const target = container.querySelector<HTMLElement>(selector);
      expect(target).not.toBeNull();
      act(() => target?.focus());
      await vi.waitFor(() => expect(routeModules[moduleName]).toBe(true));
    };

    await preloadByPointer('.lux-desktop-nav a[href="/libraries"]', "libraries");
    // Shell routes only load their chunk on hover; the console entry also warms its query, so the
    // dashboard does not wait for layout chunk -> page chunk -> request in sequence.
    expect(prefetchQuery).not.toHaveBeenCalled();
    await preloadByFocus('.lux-desktop-nav a[href="/favorites"]', "favorites");
    await preloadByFocus('.lux-desktop-nav a[href="/admin"]', "admin");
    expect(prefetchQuery).toHaveBeenCalledWith(
      expect.objectContaining({ queryKey: queryKeys.adminDashboard }),
    );
    await preloadByPointer(".lux-user-button", "account");

    act(() => container.querySelector<HTMLButtonElement>(".lux-menu-button")?.click());
    await vi.waitFor(() => expect(container.querySelector(".lux-mobile-nav")).not.toBeNull());
    await preloadByFocus('.lux-mobile-nav a[href="/search"]', "search");
  });
});
