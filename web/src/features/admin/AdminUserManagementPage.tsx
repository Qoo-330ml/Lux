import { ArrowLeft, CircleDashed, ShieldCheck, SlidersHorizontal, Users } from "lucide-react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { FormEvent, useEffect, useState } from "react";
import { Link, useParams, useSearchParams } from "react-router-dom";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminUser } from "../../lib/api/types";
import "./user-management.css";

const detailSections = [
  { id: "profile", label: "资料与状态", module: "模块一", icon: Users },
  { id: "permissions", label: "权限", module: "模块四", icon: ShieldCheck },
  { id: "preferences", label: "偏好", module: "模块四", icon: SlidersHorizontal },
  { id: "content", label: "内容限制", module: "模块五", icon: ShieldCheck },
  { id: "devices", label: "设备与登录", module: "模块七", icon: Users },
  { id: "playback", label: "播放与转码", module: "模块六", icon: CircleDashed },
  { id: "activity", label: "活动记录", module: "模块七", icon: CircleDashed },
  { id: "subtitles", label: "外挂字幕", module: "模块八", icon: CircleDashed },
] as const;

const settingsSections = [
  { id: "registration", label: "注册与邀请", module: "模块二" },
  { id: "mail", label: "邮件", module: "模块二" },
  { id: "defaults", label: "新用户默认权限", module: "模块四" },
  { id: "presets", label: "角色预设", module: "模块四" },
  { id: "blocklist", label: "全站黑名单", module: "模块七" },
  { id: "retention", label: "删除与活动保留", module: "模块一 / 七" },
  { id: "proxy", label: "可信代理", module: "模块七" },
] as const;

export function AdminUserDetailPage() {
  const { userId = "" } = useParams();
  const [searchParams, setSearchParams] = useSearchParams();
  const sectionId = searchParams.get("section") ?? "profile";
  const section = detailSections.find((item) => item.id === sectionId) ?? detailSections[0];
  const userQuery = useQuery({
    queryKey: ["admin", "user", userId],
    queryFn: () => api.adminUser(userId),
    enabled: Boolean(userId),
  });
  const user = userQuery.data?.user;

  if (userQuery.isPending) return <AdminUserManagementState label="正在读取账户资料…" />;
  if (userQuery.error) return <AdminUserManagementState label={userQuery.error.message || "暂时无法读取账户资料。"} error />;
  if (!user) return <AdminUserManagementState label="找不到此账户，或账户已不存在。" error />;

  const displayName = user.displayName || user.usernameNormalized;
  const Icon = section.icon;

  return (
    <div className="lux-admin-page lux-admin-user-management-page">
      <Link className="lux-admin-user-back" to="/admin/users"><ArrowLeft size={15} />返回用户列表</Link>
      <header className="lux-admin-user-page-heading">
        <div>
          <span className="lux-admin-eyebrow">{user.usernameNormalized}</span>
          <h1>{displayName}</h1>
          <p>用户详情与访问管理</p>
        </div>
        <span className={user.isDisabled ? "lux-user-badge is-warn" : "lux-user-badge is-ok"}>{user.isDisabled ? "已禁用" : "正常"}</span>
      </header>
      <nav className="lux-admin-user-section-nav" aria-label="用户详情模块">
        {detailSections.map((item) => (
          <button key={item.id} className={item.id === section.id ? "is-active" : undefined} type="button" aria-current={item.id === section.id ? "page" : undefined} onClick={() => setSearchParams({ section: item.id })}>
            <span>{item.label}</span><small>{item.module}</small>
          </button>
        ))}
      </nav>
      {section.id === "profile" ? (
        <AdminUserProfileEditor key={user.id} user={user} />
      ) : (
        <section className="lux-admin-user-module-placeholder" aria-labelledby="lux-admin-user-module-title">
          <span className="lux-admin-user-module-icon"><Icon size={19} /></span>
          <div>
            <span className="lux-admin-eyebrow">{section.module}</span>
            <h2 id="lux-admin-user-module-title">{section.label}</h2>
            <p>页面结构已预留。此模块将在对应的用户管理任务中接入服务端数据和操作。</p>
          </div>
        </section>
      )}
    </div>
  );
}

function AdminUserProfileEditor({ user }: { user: AdminUser }) {
  const queryClient = useQueryClient();
  const [displayName, setDisplayName] = useState(user.displayName ?? "");
  const [email, setEmail] = useState(user.email ?? "");
  const [notice, setNotice] = useState("");
  const update = useMutation({
    mutationFn: () => api.updateAdminUserProfile(user.id, { displayName, email }),
    onSuccess: ({ user: updated }) => {
      setDisplayName(updated.displayName ?? "");
      setEmail(updated.email ?? "");
      setNotice("账户资料已保存。");
      queryClient.setQueryData(["admin", "user", user.id], { user: updated });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminUsers });
    },
    onError: (error) => setNotice(error.message || "资料保存失败，请重试。"),
  });
  useEffect(() => {
    setDisplayName(user.displayName ?? "");
    setEmail(user.email ?? "");
    setNotice("");
  }, [user.displayName, user.email, user.id]);

  const emailStatus = user.email
    ? user.emailVerifiedAt ? "已验证" : "未验证"
    : "未填写";
  const hasChanges = displayName !== (user.displayName ?? "") || email !== (user.email ?? "");

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setNotice("");
    update.mutate();
  }

  return (
    <section className="lux-admin-panel lux-admin-user-profile-panel" aria-labelledby="lux-admin-user-profile-title">
      <div className="lux-admin-panel-heading">
        <div><h2 id="lux-admin-user-profile-title">资料与状态</h2></div>
        <span className={user.isDisabled ? "lux-user-badge is-warn" : "lux-user-badge is-ok"}>{user.isDisabled ? "已禁用" : "正常"}</span>
      </div>
      <form className="lux-admin-form lux-admin-user-profile-form" onSubmit={submit}>
        <label>
          显示名称
          <input value={displayName} onChange={(event) => setDisplayName(event.target.value)} maxLength={128} />
        </label>
        <label>
          邮箱
          <input type="email" value={email} onChange={(event) => setEmail(event.target.value)} autoComplete="email" />
        </label>
        <p className="lux-admin-muted">
          邮箱状态：{emailStatus}。邮箱变更会清除验证状态；验证流程将在邮件功能中接入。
        </p>
        <div className="lux-admin-user-profile-actions">
          <button className="lux-button lux-button-primary" type="submit" disabled={!hasChanges || update.isPending}>
            {update.isPending ? "保存中…" : "保存资料"}
          </button>
          {notice ? <span role={update.isError ? "alert" : "status"}>{notice}</span> : null}
        </div>
      </form>
    </section>
  );
}

export function AdminUserSettingsPage() {
  const [searchParams, setSearchParams] = useSearchParams();
  const sectionId = searchParams.get("section") ?? "registration";
  const section = settingsSections.find((item) => item.id === sectionId) ?? settingsSections[0];

  return (
    <div className="lux-admin-page lux-admin-user-management-page">
      <Link className="lux-admin-user-back" to="/admin/users"><ArrowLeft size={15} />返回用户列表</Link>
      <header className="lux-admin-user-page-heading">
        <div><span className="lux-admin-eyebrow">用户管理</span><h1>全局设置</h1><p>注册方式、默认权限、安全规则和数据保留。</p></div>
      </header>
      <nav className="lux-admin-user-section-nav" aria-label="用户全局设置">
        {settingsSections.map((item) => (
          <button key={item.id} className={item.id === section.id ? "is-active" : undefined} type="button" aria-current={item.id === section.id ? "page" : undefined} onClick={() => setSearchParams({ section: item.id })}>
            <span>{item.label}</span><small>{item.module}</small>
          </button>
        ))}
      </nav>
      <section className="lux-admin-user-module-placeholder" aria-labelledby="lux-admin-user-settings-title">
        <span className="lux-admin-user-module-icon"><SlidersHorizontal size={19} /></span>
        <div><span className="lux-admin-eyebrow">{section.module}</span><h2 id="lux-admin-user-settings-title">{section.label}</h2><p>全局设置结构已预留，配置和保存能力将在对应任务中实现。</p></div>
      </section>
    </div>
  );
}

function AdminUserManagementState({ label, error = false }: { label: string; error?: boolean }) {
  return <section className="lux-admin-page-state" role={error ? "alert" : "status"}><h1>{error ? "无法打开用户详情" : "正在加载用户详情"}</h1><p>{label}</p><Link className="lux-button lux-button-secondary" to="/admin/users">返回用户列表</Link></section>;
}
