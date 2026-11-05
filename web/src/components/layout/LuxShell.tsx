import {
  Heart,
  History,
  Home,
  Library,
  Menu,
  Search,
  Settings2,
  X,
} from "lucide-react";
import { useQueryClient } from "@tanstack/react-query";
import { createContext, useContext, useEffect, useState } from "react";
import { NavLink, Outlet, useLocation, useNavigate } from "react-router-dom";
import { prefetchAdminPage } from "../../features/admin/admin-navigation";
import { api } from "../../lib/api/client";
import type { LuxUser } from "../../lib/api/types";
import { applyAccountAccent, applyAccountTheme, readAccountSettings } from "../../features/account/account-settings";
import { ScanActivityPopover } from "../../features/activity/ScanActivityPopover";
import { SseConnectionContext, useUserEvents } from "../../features/activity/useUserEvents";
import { LuxLogo } from "../LuxLogo";
import { LuxIconButton } from "../ui";

const DEFAULT_BROWSER_TITLE = "Lux Server - Lux";

const shellRouteLoaders: Record<string, () => Promise<unknown>> = {
  "/libraries": () => import("../../features/library/LibrariesPage"),
  "/favorites": () => import("../../features/favorites/FavoritesPage"),
  "/history": () => import("../../features/history/WatchHistoryPage"),
  "/search": () => import("../../features/search/SearchPage"),
  "/account": () => import("../../features/account/AccountPage"),
  "/admin": () => import("../../features/admin/AdminLayout"),
};

type LuxShellProps = { user: LuxUser; serverName?: string | null };
type AvatarContextValue = {
  avatarUrl: string | null;
  setAvatarUrl: (url: string) => void;
};

const AvatarContext = createContext<AvatarContextValue | null>(null);
const EMPTY_AVATAR_CONTEXT: AvatarContextValue = { avatarUrl: null, setAvatarUrl: () => {} };

export function useAvatar(): AvatarContextValue {
  return useContext(AvatarContext) ?? EMPTY_AVATAR_CONTEXT;
}

export function LuxShell({ user, serverName }: LuxShellProps) {
  const sseConnectionState = useUserEvents();
  const navigate = useNavigate();
  const location = useLocation();
  const queryClient = useQueryClient();
  // Hovering the admin entry also warms the dashboard page chunk and its data, so opening the
  // console does not wait for layout chunk -> page chunk -> data one after another.
  const preload = (to: string) => {
    preloadShellRoute(to);
    if (to === "/admin") prefetchAdminPage(queryClient, to);
  };
  const [mobileOpen, setMobileOpen] = useState(false);
  const [avatarUrl, setAvatarUrl] = useState<string | null>(() => api.avatarUrl());
  const [avatarImageFailed, setAvatarImageFailed] = useState(false);
  const isDetail = location.pathname.startsWith("/libraries/")
    || location.pathname.startsWith("/items/")
    || location.pathname.startsWith("/people/");
  const isHome = location.pathname === "/";

  useEffect(() => {
    const settings = readAccountSettings(user.id);
    applyAccountTheme(settings.theme);
    applyAccountAccent(settings.accentColor);
  }, [user.id]);

  useEffect(() => {
    setAvatarUrl(api.avatarUrl());
    setAvatarImageFailed(false);
  }, [user.id]);

  useEffect(() => {
    setAvatarImageFailed(false);
  }, [avatarUrl]);

  useEffect(() => {
    if (typeof document === "undefined") return;
    if (isDetail) return;

    const name = serverName?.trim();
    const serverTitle = name ? `${name} - Lux` : DEFAULT_BROWSER_TITLE;
    document.title = serverTitle;
    return () => {
      document.title = serverTitle;
    };
  }, [isDetail, serverName]);

  return (
    <SseConnectionContext.Provider value={sseConnectionState}>
    <AvatarContext.Provider value={{ avatarUrl, setAvatarUrl }}>
      <div className={isHome ? "lux-app is-home-route" : "lux-app"}>
        <header className="lux-header">
          <div className="lux-header-left">
            <NavLink className="lux-brand" to="/" aria-label="Lux 首页">
              <LuxLogo className="lux-brand-logo" />
              <span className="lux-brand-name">Lux</span>
            </NavLink>
            <nav className="lux-desktop-nav" aria-label="主导航">
              <NavLink className={navClass} to="/">
                首页
              </NavLink>
              <NavLink className={navClass} to="/libraries" onPointerEnter={() => preload("/libraries")} onFocus={() => preload("/libraries")}>
                媒体库
              </NavLink>
              <NavLink className={navClass} to="/favorites" onPointerEnter={() => preload("/favorites")} onFocus={() => preload("/favorites")}>
                收藏
              </NavLink>
              <NavLink className={navClass} to="/history" onPointerEnter={() => preload("/history")} onFocus={() => preload("/history")}>
                历史
              </NavLink>
              <NavLink className={navClass} to="/search" onPointerEnter={() => preload("/search")} onFocus={() => preload("/search")}>
                搜索
              </NavLink>
              {user.canManageServer ? (
                <NavLink className={navClass} to="/admin" onPointerEnter={() => preload("/admin")} onFocus={() => preload("/admin")}>
                  管理
                </NavLink>
              ) : null}
            </nav>
          </div>

          <div className="lux-header-actions">
            {user.canManageServer ? <ScanActivityPopover /> : null}
            <button
              className="lux-user-button"
              type="button"
              aria-label="打开账户"
              onPointerEnter={() => preload("/account")}
              onFocus={() => preload("/account")}
              onClick={() => navigate(`/account/${encodeURIComponent(user.id)}`)}
            >
              <span className="lux-avatar">
                {avatarUrl && !avatarImageFailed ? (
                  <img src={avatarUrl} alt="" onError={() => setAvatarImageFailed(true)} />
                ) : (
                  (user.displayName || user.usernameNormalized || "L").slice(0, 1).toUpperCase()
                )}
              </span>
            </button>
            <LuxIconButton className="lux-menu-button"
              type="button"
              aria-label={mobileOpen ? "关闭导航" : "打开导航"}
              aria-expanded={mobileOpen}
              onClick={() => setMobileOpen((open) => !open)}
            >
              {mobileOpen ? <X size={20} /> : <Menu size={20} />}
            </LuxIconButton>
          </div>
        </header>

        <nav
          className="lux-mobile-nav"
          data-state={mobileOpen ? "open" : "closed"}
          aria-label="移动端导航"
          aria-hidden={!mobileOpen}
          inert={!mobileOpen}
        >
          <NavLink className={mobileNavClass} to="/" onClick={() => setMobileOpen(false)}>
            <span className="lux-mobile-nav-icon" aria-hidden="true"><Home size={18} /></span>
            <span className="lux-mobile-nav-label">首页</span>
          </NavLink>
          <NavLink className={mobileNavClass} to="/libraries" onPointerEnter={() => preload("/libraries")} onFocus={() => preload("/libraries")} onClick={() => setMobileOpen(false)}>
            <span className="lux-mobile-nav-icon" aria-hidden="true"><Library size={18} /></span>
            <span className="lux-mobile-nav-label">媒体库</span>
          </NavLink>
          <NavLink className={mobileNavClass} to="/favorites" onPointerEnter={() => preload("/favorites")} onFocus={() => preload("/favorites")} onClick={() => setMobileOpen(false)}>
            <span className="lux-mobile-nav-icon" aria-hidden="true"><Heart size={18} /></span>
            <span className="lux-mobile-nav-label">收藏</span>
          </NavLink>
          <NavLink className={mobileNavClass} to="/history" onPointerEnter={() => preload("/history")} onFocus={() => preload("/history")} onClick={() => setMobileOpen(false)}>
            <span className="lux-mobile-nav-icon" aria-hidden="true"><History size={18} /></span>
            <span className="lux-mobile-nav-label">历史</span>
          </NavLink>
          <NavLink className={mobileNavClass} to="/search" onPointerEnter={() => preload("/search")} onFocus={() => preload("/search")} onClick={() => setMobileOpen(false)}>
            <span className="lux-mobile-nav-icon" aria-hidden="true"><Search size={18} /></span>
            <span className="lux-mobile-nav-label">搜索</span>
          </NavLink>
          {user.canManageServer ? (
            <NavLink className={mobileNavClass} to="/admin" onPointerEnter={() => preload("/admin")} onFocus={() => preload("/admin")} onClick={() => setMobileOpen(false)}>
              <span className="lux-mobile-nav-icon" aria-hidden="true"><Settings2 size={18} /></span>
              <span className="lux-mobile-nav-label">管理控制台</span>
            </NavLink>
          ) : null}
        </nav>

        <main className={isDetail ? "lux-main lux-main-detail" : "lux-main"}>
          <Outlet />
        </main>
      </div>
    </AvatarContext.Provider>
    </SseConnectionContext.Provider>
  );
}

function navClass({ isActive }: { isActive: boolean }) {
  return isActive ? "lux-nav-link is-active" : "lux-nav-link";
}

function mobileNavClass({ isActive }: { isActive: boolean }) {
  return isActive ? "lux-nav-link lux-mobile-nav-link is-active" : "lux-nav-link lux-mobile-nav-link";
}

function preloadShellRoute(to: string) {
  const load = shellRouteLoaders[to];
  if (load) void load().catch(() => undefined);
}
