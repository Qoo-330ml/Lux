import { AnimatePresence, motion } from "framer-motion";
import {
  Menu,
  Settings2,
  UserRound,
  X,
} from "lucide-react";
import { useEffect, useState } from "react";
import { NavLink, Outlet, useLocation, useNavigate } from "react-router-dom";
import type { LuxUser } from "../../lib/api/types";
import { applyAccountAccent, applyAccountTheme, readAccountSettings } from "../../features/account/account-settings";

type LuxShellProps = { user: LuxUser };

export function LuxShell({ user }: LuxShellProps) {
  const navigate = useNavigate();
  const location = useLocation();
  const [mobileOpen, setMobileOpen] = useState(false);
  const isDetail = location.pathname.startsWith("/libraries/");
  const isHome = location.pathname === "/";

  useEffect(() => {
    const settings = readAccountSettings(user.id);
    applyAccountTheme(settings.theme);
    applyAccountAccent(settings.accentColor);
  }, [user.id]);

  return (
    <div className={isHome ? "lux-app is-home-route" : "lux-app"}>
      <header className="lux-header">
        <div className="lux-header-left">
          <NavLink className="lux-brand" to="/" aria-label="Lux 首页">
            <img className="lux-brand-logo" src="/logo.svg" alt="" aria-hidden="true" />
            <span className="lux-brand-name">Lux</span>
          </NavLink>
          <nav className="lux-desktop-nav" aria-label="主导航">
            <NavLink className={navClass} to="/">
              首页
            </NavLink>
            <NavLink className={navClass} to="/libraries">
              媒体库
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
            onClick={() => navigate("/account")}
          >
            <span className="lux-avatar">
              {(user.displayName || user.usernameNormalized || "L")
                .slice(0, 1)
                .toUpperCase()}
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
            <NavLink className={navClass} to="/" onClick={() => setMobileOpen(false)}>
              首页
            </NavLink>
            <NavLink className={navClass} to="/libraries" onClick={() => setMobileOpen(false)}>
              媒体库
            </NavLink>
            <NavLink className={navClass} to="/search" onClick={() => setMobileOpen(false)}>
              搜索
            </NavLink>
            {user.canManageServer ? (
              <NavLink className={navClass} to="/admin" onClick={() => setMobileOpen(false)}><Settings2 size={15} /> 管理控制台</NavLink>
            ) : null}
          </motion.nav>
        ) : null}
      </AnimatePresence>

      <main className={isDetail ? "lux-main lux-main-detail" : "lux-main"}>
        <Outlet />
      </main>
    </div>
  );
}

function navClass({ isActive }: { isActive: boolean }) {
  return isActive ? "lux-nav-link is-active" : "lux-nav-link";
}
