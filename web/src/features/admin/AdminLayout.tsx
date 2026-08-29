import { Activity, BellRing, BookOpen, Database, FileClock, PackageOpen, Settings2, ShieldCheck, Users } from "lucide-react";
import { useQueryClient } from "@tanstack/react-query";
import { NavLink, Outlet } from "react-router-dom";
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
  { to: "/admin/changelog", label: "更新日志", icon: BookOpen },
];

export function AdminLayout() {
  const queryClient = useQueryClient();
  useAdminEvents();

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
      <div className="lux-admin-content"><Outlet /></div>
    </section>
  );
}

function adminNavClass({ isActive }: { isActive: boolean }) {
  return isActive ? "lux-admin-nav-link is-active" : "lux-admin-nav-link";
}
