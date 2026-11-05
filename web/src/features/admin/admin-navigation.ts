import type { QueryClient } from "@tanstack/react-query";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";

export const loadAdminDashboardPage = () => import("./AdminDashboardPage").then(({ AdminDashboardPage }) => ({ default: AdminDashboardPage }));
export const loadAdminLibrariesPage = () => import("./AdminLibrariesPage").then(({ AdminLibrariesPage }) => ({ default: AdminLibrariesPage }));
export const loadAdminPluginsPage = () => import("./AdminPluginsPage").then(({ AdminPluginsPage }) => ({ default: AdminPluginsPage }));
export const loadAdminNotificationsPage = () => import("./AdminNotificationsPage").then(({ AdminNotificationsPage }) => ({ default: AdminNotificationsPage }));
export const loadAdminUsersPage = () => import("./AdminUsersPage").then(({ AdminUsersPage }) => ({ default: AdminUsersPage }));
export const loadAdminOperationsPage = () => import("./AdminOperationsPage").then(({ AdminOperationsPage }) => ({ default: AdminOperationsPage }));
export const loadAdminSettingsPage = () => import("./AdminSettingsPage").then(({ AdminSettingsPage }) => ({ default: AdminSettingsPage }));
export const loadAdminTranscodingPage = () => import("./AdminTranscodingPage").then(({ AdminTranscodingPage }) => ({ default: AdminTranscodingPage }));
export const loadAdminChangelogPage = () => import("./AdminChangelogPage").then(({ AdminChangelogPage }) => ({ default: AdminChangelogPage }));

const adminPageLoaders: Record<string, () => Promise<unknown>> = {
  "/admin": loadAdminDashboardPage,
  "/admin/libraries": loadAdminLibrariesPage,
  "/admin/plugins": loadAdminPluginsPage,
  "/admin/notifications": loadAdminNotificationsPage,
  "/admin/users": loadAdminUsersPage,
  "/admin/jobs": loadAdminOperationsPage,
  "/admin/settings": loadAdminSettingsPage,
  "/admin/transcoding": loadAdminTranscodingPage,
  "/admin/changelog": loadAdminChangelogPage,
};

export function preloadAdminPage(to: string): Promise<void> {
  const loader = adminPageLoaders[to];
  return loader ? loader().then(() => undefined) : Promise.resolve();
}

/**
 * Normalized admin console path for a browser entry, or null when the entry is not the console.
 * Trailing slashes are dropped so `/admin/` and `/admin/users/` match the loader table, and the
 * check is a path segment match, not a prefix match: `/administrator` is not the console.
 */
export function adminEntryPath(pathname: string): string | null {
  const normalized = pathname.replace(/\/+$/, "");
  if (normalized === "/admin" || normalized.startsWith("/admin/")) {
    return normalized;
  }
  return null;
}

export function prefetchAdminPage(queryClient: QueryClient, to: string): void {
  void preloadAdminPage(to);

  switch (to) {
    case "/admin":
      void queryClient.prefetchQuery({
        queryKey: queryKeys.adminDashboard,
        queryFn: () => api.adminDashboard(),
      });
      break;
    case "/admin/libraries":
      void queryClient.prefetchQuery({
        queryKey: queryKeys.adminLibraries,
        queryFn: () => api.adminLibraries(),
      });
      void queryClient.prefetchQuery({
        queryKey: queryKeys.libraryOrder,
        queryFn: () => api.libraryOrder(),
      });
      break;
    case "/admin/plugins":
      void queryClient.prefetchQuery({
        queryKey: queryKeys.adminPlugins,
        queryFn: () => api.adminPlugins(),
      });
      break;
    case "/admin/notifications":
      void queryClient.prefetchQuery({
        queryKey: queryKeys.adminWebhookDestinations,
        queryFn: () => api.adminWebhookDestinations(),
      });
      break;
    case "/admin/users":
      void queryClient.prefetchQuery({
        queryKey: queryKeys.adminUsers,
        queryFn: () => api.adminUsers(),
      });
      break;
    case "/admin/jobs":
      void queryClient.prefetchQuery({
        queryKey: queryKeys.adminScheduledTasks(1),
        queryFn: () => api.adminScheduledTasks(1),
      });
      break;
    case "/admin/settings":
      void queryClient.prefetchQuery({
        queryKey: queryKeys.adminSettings,
        queryFn: () => api.adminSettings(),
      });
      break;
    case "/admin/transcoding":
      void queryClient.prefetchQuery({
        queryKey: queryKeys.adminSettings,
        queryFn: () => api.adminSettings(),
      });
      void queryClient.prefetchQuery({
        queryKey: queryKeys.adminTranscodingSessions,
        queryFn: () => api.adminTranscodingSessions(),
      });
      break;
    case "/admin/changelog":
      break;
    default:
      break;
  }
}
