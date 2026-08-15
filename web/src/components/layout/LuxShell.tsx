import { AnimatePresence, motion } from "framer-motion";
import {
  Heart,
  Home,
  Library,
  Menu,
  Search,
  Settings2,
  X,
} from "lucide-react";
import { createContext, useContext, useEffect, useState } from "react";
import { NavLink, Outlet, useLocation, useNavigate } from "react-router-dom";
import { api } from "../../lib/api/client";
import type { LuxUser } from "../../lib/api/types";
import { applyAccountAccent, applyAccountTheme, readAccountSettings } from "../../features/account/account-settings";
import { LuxLogo } from "../LuxLogo";

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
  const navigate = useNavigate();
  const location = useLocation();
  const [mobileOpen, setMobileOpen] = useState(false);
  const [avatarUrl, setAvatarUrl] = useState<string | null>(() => api.avatarUrl());
  const [avatarImageFailed, setAvatarImageFailed] = useState(false);
  const isDetail = location.pathname.startsWith("/libraries/");
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
    document.title = name ? `${name} - Lux` : "Lux";
    return () => {
      document.title = "Lux";
    };
  }, [isDetail, serverName]);

  return (
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
              <NavLink className={navClass} to="/libraries">
                媒体库
              </NavLink>
              <NavLink className={navClass} to="/favorites">
                收藏
              </NavLink>
              <NavLink className={navClass} to="/search">
                搜索
              </NavLink>
              {user.canManageServer ? <NavLink className={navClass} to="/admin">管理</NavLink> : null}
            </nav>
          </div>

          <div className="lux-header-actions">
            <button
              className="lux-user-button"
              type="button"
              aria-label="打开账户"
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
            <button
              className="lux-icon-button lux-menu-button"
              type="button"
              aria-label={mobileOpen ? "关闭导航" : "打开导航"}
              aria-expanded={mobileOpen}
              onClick={() => setMobileOpen((open) => !open)}
            >
              {mobileOpen ? <X size={20} /> : <Menu size={20} />}
            </button>
          </div>
        </header>

        <AnimatePresence>
          {mobileOpen ? (
            <motion.nav
              className="lux-mobile-nav"
              aria-label="移动端导航"
              initial={{ opacity: 0, y: -12 }}
              animate={{ opacity: 1, y: 0 }}
              exit={{ opacity: 0, y: -12 }}
              transition={{ duration: 0.18 }}
            >
              <NavLink className={mobileNavClass} to="/" onClick={() => setMobileOpen(false)}>
                <span className="lux-mobile-nav-icon" aria-hidden="true"><Home size={18} /></span>
                <span className="lux-mobile-nav-label">首页</span>
              </NavLink>
              <NavLink className={mobileNavClass} to="/libraries" onClick={() => setMobileOpen(false)}>
                <span className="lux-mobile-nav-icon" aria-hidden="true"><Library size={18} /></span>
                <span className="lux-mobile-nav-label">媒体库</span>
              </NavLink>
              <NavLink className={mobileNavClass} to="/favorites" onClick={() => setMobileOpen(false)}>
                <span className="lux-mobile-nav-icon" aria-hidden="true"><Heart size={18} /></span>
                <span className="lux-mobile-nav-label">收藏</span>
              </NavLink>
              <NavLink className={mobileNavClass} to="/search" onClick={() => setMobileOpen(false)}>
                <span className="lux-mobile-nav-icon" aria-hidden="true"><Search size={18} /></span>
                <span className="lux-mobile-nav-label">搜索</span>
              </NavLink>
              {user.canManageServer ? (
                <NavLink className={mobileNavClass} to="/admin" onClick={() => setMobileOpen(false)}>
                  <span className="lux-mobile-nav-icon" aria-hidden="true"><Settings2 size={18} /></span>
                  <span className="lux-mobile-nav-label">管理控制台</span>
                </NavLink>
              ) : null}
            </motion.nav>
          ) : null}
        </AnimatePresence>

        <main className={isDetail ? "lux-main lux-main-detail" : "lux-main"}>
          <Outlet />
        </main>
      </div>
    </AvatarContext.Provider>
  );
}

function navClass({ isActive }: { isActive: boolean }) {
  return isActive ? "lux-nav-link is-active" : "lux-nav-link";
}

function mobileNavClass({ isActive }: { isActive: boolean }) {
  return isActive ? "lux-nav-link lux-mobile-nav-link is-active" : "lux-nav-link lux-mobile-nav-link";
}
