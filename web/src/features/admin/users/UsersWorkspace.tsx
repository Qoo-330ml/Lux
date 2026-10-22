import { Clock, KeyRound, Mail, MailOpen, ShieldCheck, Trash2, UserCheck, Users } from "lucide-react";
import type { ReactNode } from "react";
import { useNavigate } from "react-router-dom";

export type UsersViewKey = "users" | "pending" | "invitations" | "deleted";

export type PolicySectionKey = "registration" | "mail" | "defaults" | "presets" | "blocklist" | "retention" | "proxy";

const OBJECT_ITEMS: Array<{ key: UsersViewKey; label: string; icon: typeof Users; href: string }> = [
  { key: "users", label: "全部用户", icon: Users, href: "/admin/users" },
  { key: "pending", label: "待审批", icon: UserCheck, href: "/admin/users?view=pending" },
  { key: "invitations", label: "邀请", icon: Mail, href: "/admin/users?view=invitations" },
  { key: "deleted", label: "回收站", icon: Trash2, href: "/admin/users?view=deleted" },
];

const POLICY_ITEMS: Array<{ key: string; label: string; icon: typeof Users; sections: PolicySectionKey[]; href: string }> = [
  { key: "registration", label: "注册与邮件", icon: MailOpen, sections: ["registration", "mail"], href: "/admin/users/settings?section=registration" },
  { key: "defaults", label: "角色与默认权限", icon: KeyRound, sections: ["defaults", "presets"], href: "/admin/users/settings?section=defaults" },
  { key: "blocklist", label: "访问安全", icon: ShieldCheck, sections: ["blocklist", "proxy"], href: "/admin/users/settings?section=blocklist" },
  { key: "retention", label: "数据保留", icon: Clock, sections: ["retention"], href: "/admin/users/settings?section=retention" },
];

export function UsersWorkspace({ activeView, activeSection, children }: { activeView?: UsersViewKey; activeSection?: PolicySectionKey; children: ReactNode }) {
  return (
    <div className="lux-users-workspace">
      <UsersWorkspaceNav activeView={activeView} activeSection={activeSection} />
      <div className="lux-users-pane">{children}</div>
    </div>
  );
}

export function UsersWorkspaceNav({ activeView, activeSection }: { activeView?: UsersViewKey; activeSection?: PolicySectionKey }) {
  const navigate = useNavigate();
  return (
    <nav className="lux-users-rail" aria-label="用户与权限分区">
      <div className="lux-users-rail-group">
        <span className="lux-users-rail-label">对象</span>
        {OBJECT_ITEMS.map(({ key, label, icon: Icon, href }) => (
          <button
            key={key}
            className="lux-users-rail-item"
            type="button"
            aria-current={activeView === key ? "page" : undefined}
            onClick={() => navigate(href)}
          >
            <Icon size={15} strokeWidth={1.8} aria-hidden="true" /><span>{label}</span>
          </button>
        ))}
      </div>
      <div className="lux-users-rail-group">
        <span className="lux-users-rail-label">策略</span>
        {POLICY_ITEMS.map(({ key, label, icon: Icon, sections, href }) => (
          <button
            key={key}
            className="lux-users-rail-item"
            type="button"
            aria-current={activeSection && sections.includes(activeSection) ? "page" : undefined}
            onClick={() => navigate(href)}
          >
            <Icon size={15} strokeWidth={1.8} aria-hidden="true" /><span>{label}</span>
          </button>
        ))}
      </div>
    </nav>
  );
}
