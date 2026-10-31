import "./admin.css";
import "./users-workspace.css";
import { Activity, BellRing, BookOpen, Cpu, Database, FileClock, PackageOpen, Settings2, ShieldCheck, Users } from "lucide-react";
import { useQueryClient } from "@tanstack/react-query";
import { MoreHorizontal } from "lucide-react";
import { Suspense, useEffect, useState } from "react";
import { NavLink, Outlet, useLocation } from "react-router-dom";
import { prefetchAdminPage } from "./admin-navigation";
import { useAdminEvents } from "./useAdminEvents";

export const adminNav = [
  { to: "/admin", label: "仪表盘", icon: Activity, end: true },
  { to: "/admin/libraries", label: "媒体库", icon: Database },
  { to: "/admin/plugins", label: "插件库", icon: PackageOpen },
  { to: "/admin/notifications", label: "通知", icon: BellRing },
  { to: "/admin/users", label: "用户与权限", icon: Users },
  { to: "/admin/jobs", label: "任务与日志", icon: FileClock },
  { to: "/admin/settings", label: "服务器设置", icon: Settings2 },
  { to: "/admin/transcoding", label: "转码", icon: Cpu },
  { to: "/admin/changelog", label: "更新日志", icon: BookOpen },
];

/** Phone bottom bar shows the first entries; the rest live in the "more" sheet. */
const primaryNavCount = 4;
const primaryNav = adminNav.slice(0, primaryNavCount);
const secondaryNav = adminNav.slice(primaryNavCount);

export function AdminLayout() {
  const queryClient = useQueryClient();
  const { pathname } = useLocation();
  const [moreOpen, setMoreOpen] = useState(false);
  useAdminEvents();

  useEffect(() => {
    setMoreOpen(false);
  }, [pathname]);

  const secondaryActive = secondaryNav.some(({ to }) => pathname === to || pathname.startsWith(`${to}/`));

  const preload = (to: string) => {
    prefetchAdminPage(queryClient, to);
  };

  return (
    <section className="lux-admin-layout">
      <aside className="lux-admin-sidebar" aria-label="管理员导航">
        <div className="lux-admin-sidebar-heading">
          <span className="lux-admin-badge"><ShieldCheck size={16} /></span>
          <div><strong>Lux 控制台</strong><small>服务器管理</small></div>
        </div>
        <nav className="lux-admin-nav">
          {adminNav.map(({ to, label, icon: Icon, end }) => (
            <NavLink
              key={to}
              to={to}
              end={end}
              className={adminNavClass}
              onPointerEnter={() => preload(to)}
              onFocus={() => preload(to)}
              onPointerDown={() => preload(to)}
            >
              <Icon size={17} strokeWidth={1.8} /><span>{label}</span>
            </NavLink>
          ))}
        </nav>
      </aside>
      <div className="lux-admin-content">
        <Suspense fallback={<AdminPageSkeleton />}>
          <Outlet />
        </Suspense>
      </div>
      <nav className="lux-admin-tabbar" aria-label="管理员导航（手机）">
        {primaryNav.map(({ to, label, icon: Icon, end }) => (
          <NavLink
            key={to}
            to={to}
            end={end}
            className={tabClass}
            onPointerDown={() => preload(to)}
          >
            <Icon size={21} strokeWidth={1.8} /><span>{label}</span>
          </NavLink>
        ))}
        <button
          type="button"
          className={`lux-admin-tab lux-admin-tab-more${moreOpen || secondaryActive ? " is-active" : ""}`}
          aria-expanded={moreOpen}
          onClick={() => setMoreOpen((open) => !open)}
        >
          <MoreHorizontal size={21} strokeWidth={1.8} /><span>更多</span>
        </button>
      </nav>
      {moreOpen ? (
        <>
          <div className="lux-admin-more-scrim" role="presentation" onClick={() => setMoreOpen(false)} />
          <div className="lux-admin-more-sheet" aria-label="更多管理">
            <span className="lux-admin-more-grab" aria-hidden="true" />
            <h2>更多管理</h2>
            <div className="lux-admin-more-grid">
              {secondaryNav.map(({ to, label, icon: Icon }) => (
                <NavLink
                  key={to}
                  to={to}
                  className={({ isActive }) => (isActive ? "lux-admin-more-item is-active" : "lux-admin-more-item")}
                  onPointerDown={() => preload(to)}
                >
                  <Icon size={22} strokeWidth={1.8} /><span>{label}</span>
                </NavLink>
              ))}
            </div>
          </div>
        </>
      ) : null}
    </section>
  );
}

function AdminPageSkeleton() {
  return (
    <div className="lux-admin-page-skeleton" role="status" aria-busy="true" aria-label="正在加载管理页面">
      <span className="lux-skeleton-line lux-admin-page-skeleton-title" aria-hidden="true" />
      <span className="lux-skeleton-line lux-admin-page-skeleton-description" aria-hidden="true" />
      <div className="lux-skeleton-row lux-admin-page-skeleton-content" aria-hidden="true" />
    </div>
  );
}

function tabClass({ isActive }: { isActive: boolean }) {
  return isActive ? "lux-admin-tab is-active" : "lux-admin-tab";
}

function adminNavClass({ isActive }: { isActive: boolean }) {
  return isActive ? "lux-admin-nav-link is-active" : "lux-admin-nav-link";
}
