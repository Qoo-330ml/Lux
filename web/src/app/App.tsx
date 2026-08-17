import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Route, Routes, Navigate, useLocation, useParams } from "react-router-dom";
import { lazy, Suspense } from "react";
import { api, ApiError } from "../lib/api/client";
import { queryKeys } from "../lib/api/query-keys";
import type { LuxUser } from "../lib/api/types";
import { LuxShell } from "../components/layout/LuxShell";
import { LoginPage } from "../features/auth/LoginPage";
import { SetupPage } from "../features/auth/SetupPage";
import { HomePage } from "../features/home/HomePage";

const LibrariesPage = lazy(() => import("../features/library/LibrariesPage").then(({ LibrariesPage }) => ({ default: LibrariesPage })));
const LibraryPage = lazy(() => import("../features/library/LibraryPage").then(({ LibraryPage }) => ({ default: LibraryPage })));
const SearchPage = lazy(() => import("../features/search/SearchPage").then(({ SearchPage }) => ({ default: SearchPage })));
const FavoritesPage = lazy(() => import("../features/favorites/FavoritesPage").then(({ FavoritesPage }) => ({ default: FavoritesPage })));
const MediaDetailPage = lazy(() => import("../features/detail/MediaDetailPage").then(({ MediaDetailPage }) => ({ default: MediaDetailPage })));
const PlayerPage = lazy(() => import("../features/player/PlayerPage").then(({ PlayerPage }) => ({ default: PlayerPage })));
const AccountPage = lazy(() => import("../features/account/AccountPage").then(({ AccountPage }) => ({ default: AccountPage })));
const AdminLayout = lazy(() => import("../features/admin/AdminLayout").then(({ AdminLayout }) => ({ default: AdminLayout })));
const AdminDashboardPage = lazy(() => import("../features/admin/AdminDashboardPage").then(({ AdminDashboardPage }) => ({ default: AdminDashboardPage })));
const AdminLibrariesPage = lazy(() => import("../features/admin/AdminLibrariesPage").then(({ AdminLibrariesPage }) => ({ default: AdminLibrariesPage })));
const AdminPluginsPage = lazy(() => import("../features/admin/AdminPluginsPage").then(({ AdminPluginsPage }) => ({ default: AdminPluginsPage })));
const AdminNotificationsPage = lazy(() => import("../features/admin/AdminNotificationsPage").then(({ AdminNotificationsPage }) => ({ default: AdminNotificationsPage })));
const AdminUsersPage = lazy(() => import("../features/admin/AdminUsersPage").then(({ AdminUsersPage }) => ({ default: AdminUsersPage })));
const AdminOperationsPage = lazy(() => import("../features/admin/AdminOperationsPage").then(({ AdminOperationsPage }) => ({ default: AdminOperationsPage })));
const AdminSettingsPage = lazy(() => import("../features/admin/AdminSettingsPage").then(({ AdminSettingsPage }) => ({ default: AdminSettingsPage })));
const AdminChangelogPage = lazy(() => import("../features/admin/AdminChangelogPage").then(({ AdminChangelogPage }) => ({ default: AdminChangelogPage })));

export function App() {
  const setup = useQuery({
    queryKey: queryKeys.setup,
    queryFn: () => api.setupStatus(),
    retry: false,
  });

  if (setup.isPending) return <LoadingScreen label="正在启动 Lux" />;
  if (setup.error) return <ErrorScreen error={setup.error} />;
  if (!setup.data.initialized) return <SetupPage />;

  return <SessionRoutes />;
}

function SessionRoutes() {
  const location = useLocation();
  const me = useQuery({
    queryKey: queryKeys.me,
    queryFn: () => api.me(),
    retry: false,
  });

  if (me.isPending) return <LoadingScreen label="正在恢复会话" />;
  if (me.error && !(me.error instanceof ApiError && me.error.status === 401)) {
    return <ErrorScreen error={me.error} />;
  }
  if (!me.data) {
    return (
      <Routes location={location}>
        <Route path="*" element={<LoginPage />} />
      </Routes>
    );
  }

  return <AuthenticatedRoutes user={me.data.user} serverName={me.data.serverName} />;
}

function AuthenticatedRoutes({ user, serverName }: { user: LuxUser; serverName?: string | null }) {
  return (
    <Suspense fallback={<LoadingScreen label="正在打开页面" />}>
      <Routes>
        <Route element={<LuxShell user={user} serverName={serverName} />}>
          <Route index element={<HomePage user={user} />} />
          <Route path="libraries" element={<LibrariesPage />} />
          <Route path="libraries/:libraryId" element={<LibraryPage serverName={serverName} />} />
          <Route path="favorites" element={<FavoritesPage />} />
          <Route path="items/:itemId" element={<MediaDetailPage />} />
          <Route path="watch/:itemId" element={<PlayerPage />} />
          <Route path="search" element={<SearchPage />} />
          <Route path="account" element={<Navigate to={`/account/${encodeURIComponent(user.id)}`} replace />} />
          <Route path="account/:userId" element={<AccountRoute user={user} />} />
          <Route path="admin" element={user.canManageServer ? <AdminLayout /> : <Navigate to="/" replace />}>
            <Route index element={<AdminDashboardPage />} />
            <Route path="libraries" element={<AdminLibrariesPage />} />
            <Route path="plugins" element={<AdminPluginsPage />} />
            <Route path="notifications" element={<AdminNotificationsPage />} />
            <Route path="users" element={<AdminUsersPage />} />
            <Route path="jobs" element={<AdminOperationsPage />} />
            <Route path="settings" element={<AdminSettingsPage />} />
            <Route path="changelog" element={<AdminChangelogPage />} />
          </Route>
          <Route path="*" element={<Navigate to="/" replace />} />
        </Route>
      </Routes>
    </Suspense>
  );
}

function AccountRoute({ user }: { user: LuxUser }) {
  const { userId } = useParams();
  if (userId !== user.id) {
    return <Navigate to={`/account/${encodeURIComponent(user.id)}`} replace />;
  }
  return <AccountPage user={user} />;
}

function LoadingScreen({ label }: { label: string }) {
  return (
    <main className="lux-state-screen" aria-busy="true">
      <div className="lux-spinner" aria-hidden="true" />
      <p>{label}</p>
    </main>
  );
}

function ErrorScreen({ error }: { error: Error }) {
  const queryClient = useQueryClient();
  return (
    <main className="lux-state-screen" role="alert">
      <h1>暂时无法连接服务器</h1>
      <p>{error.message}</p>
      <button
        className="lux-button lux-button-secondary"
        type="button"
        onClick={() => void queryClient.invalidateQueries()}
      >
        重试
      </button>
    </main>
  );
}
