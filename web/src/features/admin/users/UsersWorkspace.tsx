import { Link, useNavigate } from "react-router-dom";
import type { ReactNode } from "react";

export type UsersViewKey = "users" | "pending" | "invitations" | "deleted";

export type PolicySectionKey = "registration" | "mail" | "defaults" | "presets" | "blocklist" | "retention" | "proxy";

const VIEW_TABS: Array<{ key: UsersViewKey; label: string; href: string }> = [
  { key: "users", label: "用户", href: "/admin/users" },
  { key: "pending", label: "待审批", href: "/admin/users?view=pending" },
  { key: "invitations", label: "邀请", href: "/admin/users?view=invitations" },
  { key: "deleted", label: "回收站", href: "/admin/users?view=deleted" },
];

const POLICY_TABS: Array<{ key: string; label: string; sections: PolicySectionKey[]; href: string }> = [
  { key: "registration", label: "注册与邮件", sections: ["registration", "mail"], href: "/admin/users/settings?section=registration" },
  { key: "defaults", label: "角色与默认权限", sections: ["defaults", "presets"], href: "/admin/users/settings?section=defaults" },
  { key: "blocklist", label: "访问安全", sections: ["blocklist", "proxy"], href: "/admin/users/settings?section=blocklist" },
  { key: "retention", label: "数据保留", sections: ["retention"], href: "/admin/users/settings?section=retention" },
];

type UsersWorkspaceProps = {
  activeView?: UsersViewKey;
  activeSection?: PolicySectionKey;
  /** 标题行右侧的主操作。 */
  actions?: ReactNode;
  children: ReactNode;
};

/**
 * 「用户与权限」统一外壳：标题行 + 一排顶部标签（用户 / 待审批 / 邀请 / 回收站 / 策略）。
 * 进入策略页时在标签下方再显示策略分组，不再使用左侧栏。
 */
export function UsersWorkspace({ activeView, activeSection, actions, children }: UsersWorkspaceProps) {
  const navigate = useNavigate();
  const inPolicy = activeSection !== undefined;
  return (
    <div className="lux-users-shell">
      <header className="lux-users-heading">
        <h1>用户与权限</h1>
        {actions ? <div className="lux-users-heading-actions">{actions}</div> : null}
      </header>
      <nav className="lux-users-tabs" aria-label="用户与权限分区">
        {VIEW_TABS.map(({ key, label, href }) => (
          <button key={key} type="button" aria-current={!inPolicy && activeView === key ? "page" : undefined} onClick={() => navigate(href)}>{label}</button>
        ))}
        <span className="lux-users-tabs-spacer" aria-hidden="true" />
        <Link className="lux-users-tabs-policy" to="/admin/users/settings" aria-current={inPolicy ? "page" : undefined}>策略与安全</Link>
      </nav>
      {inPolicy ? (
        <nav className="lux-users-chips lux-users-subnav" aria-label="策略分组">
          {POLICY_TABS.map(({ key, label, sections, href }) => (
            <button key={key} type="button" aria-pressed={sections.includes(activeSection)} onClick={() => navigate(href)}>{label}</button>
          ))}
        </nav>
      ) : null}
      <div className="lux-users-pane">{children}</div>
    </div>
  );
}
