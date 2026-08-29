import type { QueryClient } from "@tanstack/react-query";
import { describe, expect, it, vi } from "vitest";
import { api } from "../src/lib/api/client";
import { queryKeys } from "../src/lib/api/query-keys";
import { prefetchAdminPage, preloadAdminPage } from "../src/features/admin/admin-navigation";

const pluginModuleState = vi.hoisted(() => ({ loaded: false }));

vi.mock("../src/features/admin/AdminPluginsPage", () => {
  pluginModuleState.loaded = true;
  return { AdminPluginsPage: () => null };
});

describe("admin navigation preloading", () => {
  it("starts loading the target page module immediately", async () => {
    pluginModuleState.loaded = false;

    const loading = preloadAdminPage("/admin/plugins");

    await vi.waitFor(() => expect(pluginModuleState.loaded).toBe(true));
    await loading;
  });

  it.each([
    ["/admin", [queryKeys.adminDashboard]],
    ["/admin/libraries", [queryKeys.adminLibraries, queryKeys.libraryOrder]],
    ["/admin/plugins", [queryKeys.adminPlugins]],
    ["/admin/notifications", [queryKeys.adminWebhookDestinations]],
    ["/admin/users", [queryKeys.adminUsers]],
    ["/admin/jobs", [queryKeys.adminScheduledTasks(1)]],
    ["/admin/settings", [queryKeys.adminSettings]],
    ["/admin/changelog", []],
  ] as const)("prefetches the main data for %s", (to, expectedKeys) => {
    const prefetchQuery = vi.fn().mockResolvedValue(undefined);
    const queryClient = { prefetchQuery } as unknown as QueryClient;

    prefetchAdminPage(queryClient, to);

    expect(prefetchQuery.mock.calls.map(([options]) => options.queryKey)).toEqual(expectedKeys);
  });

  it("does not prefetch data for an unknown route", () => {
    const prefetchQuery = vi.fn().mockResolvedValue(undefined);
    const queryClient = { prefetchQuery } as unknown as QueryClient;

    prefetchAdminPage(queryClient, "/admin/unknown");

    expect(prefetchQuery).not.toHaveBeenCalled();
  });

  it("uses the existing API client for the prefetched query functions", async () => {
    const adminPlugins = vi.spyOn(api, "adminPlugins").mockResolvedValue({ plugins: [] });
    const queryClient = new (class {
      async prefetchQuery(options: { queryFn: () => Promise<unknown> }) {
        return options.queryFn();
      }
    })() as unknown as QueryClient;

    prefetchAdminPage(queryClient, "/admin/plugins");

    await vi.waitFor(() => expect(adminPlugins).toHaveBeenCalledTimes(1));
  });
});
