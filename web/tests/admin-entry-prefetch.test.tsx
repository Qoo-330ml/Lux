// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/app/App";
import { adminEntryPath } from "../src/features/admin/admin-navigation";
import { api } from "../src/lib/api/client";
import { queryKeys } from "../src/lib/api/query-keys";

vi.mock("../src/features/admin/AdminLayout", () => ({
  AdminLayout: () => <div data-testid="admin-layout" />,
}));
// Mock every admin page so a query can only be issued by the entry prefetch, never by rendering.
vi.mock("../src/features/admin/AdminDashboardPage", () => ({ AdminDashboardPage: () => null }));
vi.mock("../src/features/admin/AdminUsersPage", () => ({ AdminUsersPage: () => null }));

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const adminUser = { id: "user-1", usernameNormalized: "admin", canManageServer: true };
const viewer = { id: "user-2", usernameNormalized: "viewer", canManageServer: false };

describe("adminEntryPath", () => {
  it.each([
    ["/admin", "/admin"],
    ["/admin/", "/admin"],
    ["/admin/users", "/admin/users"],
    ["/admin/users/", "/admin/users"],
    ["/admin/users/42/detail", "/admin/users/42/detail"],
    ["/administrator", null],
    ["/Admin", null],
    ["/libraries", null],
    ["/", null],
    ["", null],
  ] as const)("maps %s to %s", (pathname, expected) => {
    expect(adminEntryPath(pathname)).toBe(expected);
  });
});

describe("admin console entry prefetch", () => {
  let container: HTMLDivElement;
  let root: Root;
  let queryClient: QueryClient;

  beforeEach(() => {
    vi.spyOn(api, "setupStatus").mockResolvedValue({ initialized: true });
    vi.spyOn(api, "homeLibraries").mockResolvedValue({ libraries: [] });
    vi.spyOn(api, "adminDashboard").mockResolvedValue({} as Awaited<ReturnType<typeof api.adminDashboard>>);
    vi.spyOn(api, "adminUsers").mockResolvedValue({} as Awaited<ReturnType<typeof api.adminUsers>>);
    queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  async function renderAt(entry: string, user: typeof adminUser | typeof viewer) {
    vi.spyOn(api, "me").mockResolvedValue({ user, serverName: "客厅 Lux" });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={[entry]}>
            <App />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
  }

  it("warms the dashboard query when the browser lands on the console root", async () => {
    await renderAt("/admin", adminUser);

    await vi.waitFor(() => expect(api.adminDashboard).toHaveBeenCalled());
    expect(queryClient.getQueryData(queryKeys.adminDashboard)).toBeDefined();
  });

  it("normalizes a trailing slash and warms the deeper console page", async () => {
    await renderAt("/admin/users/", adminUser);

    await vi.waitFor(() => expect(api.adminUsers).toHaveBeenCalled());
    expect(queryClient.getQueryData(queryKeys.adminUsers)).toBeDefined();
  });

  it("does not prefetch console data for a non-console entry", async () => {
    await renderAt("/", adminUser);

    await vi.waitFor(() => expect(api.homeLibraries).toHaveBeenCalled());
    expect(api.adminDashboard).not.toHaveBeenCalled();
    expect(api.adminUsers).not.toHaveBeenCalled();
  });

  it("does not prefetch console data for an account without server management", async () => {
    await renderAt("/admin", viewer);

    await vi.waitFor(() => expect(api.homeLibraries).toHaveBeenCalled());
    expect(api.adminDashboard).not.toHaveBeenCalled();
  });
});
