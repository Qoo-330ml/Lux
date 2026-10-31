import { useQuery, useQueryClient, type UseQueryResult } from "@tanstack/react-query";
import { Route, Routes, Navigate, useLocation, useParams } from "react-router-dom";
import { Component, lazy, Suspense, useEffect } from "react";
import type { ReactNode } from "react";
import { api, ApiError } from "../lib/api/client";
import { queryKeys } from "../lib/api/query-keys";
import type { LuxUser } from "../lib/api/types";
import { LuxShell } from "../components/layout/LuxShell";
import { attachTouchCardHoverTracking } from "./touch-card-hover";
import { LoginPage } from "../features/auth/LoginPage";
import { AcceptInvitationPage } from "../features/auth/AcceptInvitationPage";
import { RegisterPage } from "../features/auth/RegisterPage";
import { SetupPage } from "../features/auth/SetupPage";
import { HomePage } from "../features/home/HomePage";
import { ImageVariantsContext } from "../features/home/image-variants";
import {
  loadAdminChangelogPage,
  loadAdminDashboardPage,
  loadAdminLibrariesPage,
  loadAdminNotificationsPage,
  loadAdminOperationsPage,
  loadAdminPluginsPage,
  loadAdminSettingsPage,
  loadAdminTranscodingPage,
  loadAdminUsersPage,
} from "../features/admin/admin-navigation";
import { LuxButton } from "../components/ui";

const LibrariesPage = lazy(() => import("../features/library/LibrariesPage").then(({ LibrariesPage }) => ({ default: LibrariesPage })));
const LibraryPage = lazy(() => import("../features/library/LibraryPage").then(({ LibraryPage }) => ({ default: LibraryPage })));
const SearchPage = lazy(() => import("../features/search/SearchPage").then(({ SearchPage }) => ({ default: SearchPage })));
const FavoritesPage = lazy(() => import("../features/favorites/FavoritesPage").then(({ FavoritesPage }) => ({ default: FavoritesPage })));
const MediaDetailPage = lazy(() => import("../features/detail/MediaDetailPage").then(({ MediaDetailPage }) => ({ default: MediaDetailPage })));
const CollectionDetailPage = lazy(() => import("../features/detail/CollectionDetailPage").then(({ CollectionDetailPage }) => ({ default: CollectionDetailPage })));
const PersonDetailPage = lazy(() => import("../features/detail/PersonDetailPage").then(({ PersonDetailPage }) => ({ default: PersonDetailPage })));
const PlayerPage = lazy(() => import("../features/player/PlayerPage").then(({ PlayerPage }) => ({ default: PlayerPage })));
const AccountPage = lazy(() => import("../features/account/AccountPage").then(({ AccountPage }) => ({ default: AccountPage })));
const AdminLayout = lazy(() => import("../features/admin/AdminLayout").then(({ AdminLayout }) => ({ default: AdminLayout })));
const AdminDashboardPage = lazy(loadAdminDashboardPage);
const AdminLibrariesPage = lazy(loadAdminLibrariesPage);
const AdminPluginsPage = lazy(loadAdminPluginsPage);
const AdminNotificationsPage = lazy(loadAdminNotificationsPage);
const AdminUsersPage = lazy(loadAdminUsersPage);
const AdminOperationsPage = lazy(loadAdminOperationsPage);
const AdminSettingsPage = lazy(loadAdminSettingsPage);
const AdminTranscodingPage = lazy(loadAdminTranscodingPage);
const AdminChangelogPage = lazy(loadAdminChangelogPage);
const AdminUserDetailPage = lazy(() => import("../features/admin/AdminUserManagementPage").then(({ AdminUserDetailPage }) => ({ default: AdminUserDetailPage })));
const AdminUserSettingsPage = lazy(() => import("../features/admin/AdminUserManagementPage").then(({ AdminUserSettingsPage }) => ({ default: AdminUserSettingsPage })));

export function App() {
  const location = useLocation();
  const pathname = location.pathname.replace(/\/+$/, "");
  if (pathname.endsWith("/register/invitation")) {
    return <AcceptInvitationPage />;
  }
  if (pathname.endsWith("/register") || pathname.endsWith("/register/verify-email")) {
    return <RegisterPage />;
  }
  return <ServerApp />;
}

function ServerApp() {
  const setup = useQuery({
    queryKey: queryKeys.setup,
    queryFn: () => api.setupStatus(),
    retry: false,
  });
  // Start the session probe in parallel with setup. On an uninitialized
  // server it fails immediately; once setup finishes the effect retries it.
  const me = useQuery({
    queryKey: queryKeys.me,
    queryFn: () => api.me(),
    retry: false,
  });

  useEffect(() => {
    if (
      setup.data?.initialized
      && me.isError
      && !(me.error instanceof ApiError && me.error.status === 401)
    ) {
      void me.refetch();
    }
  }, [me.error, me.isError, me.refetch, setup.data?.initialized]);

  if (setup.isPending) return <LoadingScreen label="正在启动 Lux" />;
  if (setup.error) return <ErrorScreen error={setup.error} />;
  if (!setup.data.initialized) return <SetupPage />;

  return <SessionRoutes me={me} />;
}

function SessionRoutes({ me }: { me: UseQueryResult<Awaited<ReturnType<typeof api.me>>, Error> }) {
  const location = useLocation();

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

  return (
    <ImageVariantsContext.Provider
      value={{
        enabled: me.data.imageVariantsEnabled === true,
      }}
    >
      <AuthenticatedRoutes user={me.data.user} serverName={me.data.serverName} />
    </ImageVariantsContext.Provider>
  );
}

function AuthenticatedRoutes({ user, serverName }: { user: LuxUser; serverName?: string | null }) {
  useEffect(() => attachTouchCardHoverTracking(document, window), []);

  return (
    <RouteErrorBoundary>
      <Suspense fallback={<LoadingScreen label="正在打开页面" />}>
        <Routes>
          <Route path="watch/:itemId" element={<PlayerPage />} />
          <Route element={<LuxShell user={user} serverName={serverName} />}>
            <Route index element={<HomePage user={user} />} />
            <Route path="libraries" element={<SuspensePage><LibrariesPage /></SuspensePage>} />
            <Route path="libraries/:libraryId" element={<SuspensePage><LibraryPage serverName={serverName} /></SuspensePage>} />
            <Route path="favorites" element={<SuspensePage><FavoritesPage /></SuspensePage>} />
            <Route path="items/:itemId" element={<SuspensePage><MediaDetailPage /></SuspensePage>} />
            <Route path="collections/:collectionId" element={<SuspensePage><CollectionDetailPage /></SuspensePage>} />
            <Route path="people/:personId" element={<SuspensePage><PersonDetailPage user={user} /></SuspensePage>} />
            <Route path="search" element={<SuspensePage><SearchPage /></SuspensePage>} />
            <Route path="account" element={<Navigate to={`/account/${encodeURIComponent(user.id)}`} replace />} />
            <Route path="account/:userId" element={<SuspensePage><AccountRoute user={user} /></SuspensePage>} />
            <Route path="admin" element={user.canManageServer
              ? <SuspensePage><AdminLayout /></SuspensePage>
              : <Navigate to="/" replace />
            }>
              <Route index element={<AdminDashboardPage />} />
              <Route path="libraries" element={<AdminLibrariesPage />} />
              <Route path="plugins" element={<AdminPluginsPage />} />
              <Route path="notifications" element={<AdminNotificationsPage />} />
              <Route path="users" element={<AdminUsersPage />} />
              <Route path="users/settings" element={<AdminUserSettingsPage />} />
              <Route path="users/:userId" element={<AdminUserDetailPage />} />
              <Route path="jobs" element={<AdminOperationsPage />} />
              <Route path="settings" element={<AdminSettingsPage />} />
              <Route path="transcoding" element={<AdminTranscodingPage />} />
              <Route path="changelog" element={<AdminChangelogPage />} />
            </Route>
          </Route>
          <Route path="*" element={<Navigate to="/" replace />} />
        </Routes>
      </Suspense>
    </RouteErrorBoundary>
  );
}

function SuspensePage({ children }: { children: ReactNode }) {
  return (
    <Suspense fallback={<PageSkeleton />}>
      {children}
    </Suspense>
  );
}

function PageSkeleton() {
  return (
    <div className="lux-route-skeleton" role="status" aria-busy="true" aria-label="正在加载页面">
      <span className="lux-skeleton-line lux-route-skeleton-title" aria-hidden="true" />
      <span className="lux-skeleton-line lux-route-skeleton-description" aria-hidden="true" />
      <div className="lux-skeleton-row lux-route-skeleton-content" aria-hidden="true" />
    </div>
  );
}

class RouteErrorBoundary extends Component<{ children: ReactNode }, { hasError: boolean }> {
  state = { hasError: false };

  static getDerivedStateFromError() {
    return { hasError: true };
  }

  render() {
    if (this.state.hasError) {
      return (
        <main className="lux-state-screen" role="alert">
          <h1>页面加载失败</h1>
          <p>页面资源可能已更新，请重新加载后继续。</p>
          <LuxButton tone="secondary"
            type="button"
            onClick={() => window.location.reload()}
          >
            重新加载
          </LuxButton>
        </main>
      );
    }
    return this.props.children;
  }
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
      <LuxButton tone="secondary"
        type="button"
        onClick={() => void queryClient.invalidateQueries()}
      >
        重试
      </LuxButton>
    </main>
  );
}
