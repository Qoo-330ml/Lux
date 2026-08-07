import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Route, Routes, Navigate, useLocation } from "react-router-dom";
import { api, ApiError } from "../lib/api/client";
import { queryKeys } from "../lib/api/query-keys";
import type { LuxUser } from "../lib/api/types";
import { LuxShell } from "../components/layout/LuxShell";
import { AccountPage } from "../features/account/AccountPage";
import { LoginPage } from "../features/auth/LoginPage";
import { SetupPage } from "../features/auth/SetupPage";
import { HomePage } from "../features/home/HomePage";
import { LibraryPage } from "../features/library/LibraryPage";
import { LibrariesPage } from "../features/library/LibrariesPage";
import { SearchPage } from "../features/search/SearchPage";
import { MediaDetailPage } from "../features/detail/MediaDetailPage";
import { PlayerPage } from "../features/player/PlayerPage";
import { AdminDashboardPage } from "../features/admin/AdminDashboardPage";
import { AdminLayout } from "../features/admin/AdminLayout";
import { AdminLibrariesPage } from "../features/admin/AdminLibrariesPage";
import { AdminPluginsPage } from "../features/admin/AdminPluginsPage";
import { AdminUsersPage } from "../features/admin/AdminUsersPage";
import { AdminOperationsPage } from "../features/admin/AdminOperationsPage";
import { AdminSettingsPage } from "../features/admin/AdminSettingsPage";
import { AdminMetadataPage } from "../features/admin/AdminMetadataPage";

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

  return <AuthenticatedRoutes user={me.data} />;
}

function AuthenticatedRoutes({ user }: { user: LuxUser }) {
  return (
    <Routes>
      <Route element={<LuxShell user={user} />}>
        <Route index element={<HomePage />} />
        <Route path="libraries" element={<LibrariesPage />} />
        <Route path="libraries/:libraryId" element={<LibraryPage />} />
        <Route path="items/:itemId" element={<MediaDetailPage />} />
        <Route path="watch/:itemId" element={<PlayerPage />} />
        <Route path="search" element={<SearchPage />} />
        <Route path="account" element={<AccountPage user={user} />} />
        <Route path="admin" element={user.canManageServer ? <AdminLayout /> : <Navigate to="/" replace />}>
          <Route index element={<AdminDashboardPage />} />
          <Route path="libraries" element={<AdminLibrariesPage />} />
          <Route path="plugins" element={<AdminPluginsPage />} />
          <Route path="users" element={<AdminUsersPage />} />
          <Route path="jobs" element={<AdminOperationsPage />} />
          <Route path="settings" element={<AdminSettingsPage />} />
          <Route path="metadata" element={<AdminMetadataPage />} />
        </Route>
        <Route path="*" element={<Navigate to="/" replace />} />
      </Route>
    </Routes>
  );
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
