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
export const loadAdminChangelogPage = () => import("./AdminChangelogPage").then(({ AdminChangelogPage }) => ({ default: AdminChangelogPage }));

const adminPageLoaders: Record<string, () => Promise<unknown>> = {
  "/admin": loadAdminDashboardPage,
  "/admin/libraries": loadAdminLibrariesPage,
  "/admin/plugins": loadAdminPluginsPage,
  "/admin/notifications": loadAdminNotificationsPage,
  "/admin/users": loadAdminUsersPage,
  "/admin/jobs": loadAdminOperationsPage,
  "/admin/settings": loadAdminSettingsPage,
  "/admin/changelog": loadAdminChangelogPage,
};

export function preloadAdminPage(to: string): Promise<void> {
  const loader = adminPageLoaders[to];
  return loader ? loader().then(() => undefined) : Promise.resolve();
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
    case "/admin/changelog":
      break;
    default:
      break;
  }
}
