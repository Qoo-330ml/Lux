import { ArrowLeft, CircleDashed, ShieldCheck, SlidersHorizontal, UserRound, Users } from "lucide-react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { FormEvent, useEffect, useState } from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router-dom";
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
  const statusLabel = user.deletedAt != null ? "回收站" : user.isDisabled ? "已禁用" : "正常";
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
        <span className={user.deletedAt != null || user.isDisabled ? "lux-user-badge is-warn" : "lux-user-badge is-ok"}>{statusLabel}</span>
      </header>
      <nav className="lux-admin-user-section-nav" aria-label="用户详情模块">
        {detailSections.map((item) => (
          <button key={item.id} className={item.id === section.id ? "is-active" : undefined} type="button" aria-current={item.id === section.id ? "page" : undefined} onClick={() => setSearchParams({ section: item.id })}>
            <span>{item.label}</span><small>{item.module}</small>
          </button>
        ))}
      </nav>
      {section.id === "profile" ? (
        <>
          <AdminUserProfileEditor key={user.id} user={user} />
          <AdminUserLifecycleActions user={user} />
        </>
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
  const [avatarNotice, setAvatarNotice] = useState("");
  const [avatarVersion, setAvatarVersion] = useState(() => String(Date.now()));
  const [avatarFailed, setAvatarFailed] = useState(false);
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
  const updateAvatar = useMutation({
    mutationFn: (file: File) => api.uploadAdminUserAvatar(user.id, file),
    onSuccess: () => {
      setAvatarVersion(String(Date.now()));
      setAvatarFailed(false);
      setAvatarNotice("头像已更新。");
    },
    onError: (error) => setAvatarNotice(error.message || "头像更新失败，请重试。"),
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
        <span className={user.deletedAt != null || user.isDisabled ? "lux-user-badge is-warn" : "lux-user-badge is-ok"}>
          {user.deletedAt != null ? "回收站" : user.isDisabled ? "已禁用" : "正常"}
        </span>
      </div>
      <form className="lux-admin-form lux-admin-user-profile-form" onSubmit={submit}>
        <div className="lux-admin-user-avatar-row">
          <span className="lux-admin-user-avatar is-profile">
            {avatarFailed ? <UserRound size={27} /> : <img src={api.adminUserAvatarUrl(user.id, avatarVersion)} alt="" onError={() => setAvatarFailed(true)} />}
          </span>
          <div className="lux-admin-user-avatar-actions">
            <strong>头像</strong>
            <label className="lux-button lux-button-secondary lux-admin-user-avatar-upload">
              <span>{updateAvatar.isPending ? "上传中…" : "更换头像"}</span>
              <input
                type="file"
                accept="image/jpeg,image/png,image/webp"
                aria-label="更换用户头像"
                disabled={user.deletedAt != null || updateAvatar.isPending}
                onChange={(event) => {
                  const file = event.currentTarget.files?.[0];
                  event.currentTarget.value = "";
                  if (file) {
                    setAvatarNotice("");
                    updateAvatar.mutate(file);
                  }
                }}
              />
            </label>
            {avatarNotice ? <span role={updateAvatar.isError ? "alert" : "status"}>{avatarNotice}</span> : null}
          </div>
        </div>
        <label>
          显示名称
          <input value={displayName} onChange={(event) => setDisplayName(event.target.value)} maxLength={128} disabled={user.deletedAt != null} />
        </label>
        <label>
          邮箱
          <input type="email" value={email} onChange={(event) => setEmail(event.target.value)} autoComplete="email" disabled={user.deletedAt != null} />
        </label>
        <p className="lux-admin-muted">{user.deletedAt != null ? "回收站中的账号资料为只读。" : `邮箱状态：${emailStatus}。邮箱变更会清除验证状态；验证流程将在邮件功能中接入。`}</p>
        <div className="lux-admin-user-profile-actions">
          <button className="lux-button lux-button-primary" type="submit" disabled={user.deletedAt != null || !hasChanges || update.isPending}>
            {update.isPending ? "保存中…" : "保存资料"}
          </button>
          {notice ? <span role={update.isError ? "alert" : "status"}>{notice}</span> : null}
        </div>
      </form>
    </section>
  );
}

function AdminUserLifecycleActions({ user }: { user: AdminUser }) {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [error, setError] = useState("");
  const refresh = (updated: AdminUser) => {
    queryClient.setQueryData(["admin", "user", user.id], { user: updated });
    void queryClient.invalidateQueries({ queryKey: queryKeys.adminUsers });
  };
  const disable = useMutation({ mutationFn: () => api.disableAdminUser(user.id), onSuccess: ({ user: updated }) => refresh(updated), onError: (cause) => setError(cause.message) });
  const enable = useMutation({ mutationFn: () => api.updateAdminUser(user.id, { isDisabled: false }), onSuccess: ({ user: updated }) => refresh(updated), onError: (cause) => setError(cause.message) });
  const remove = useMutation({ mutationFn: () => api.deleteAdminUser(user.id), onSuccess: ({ user: updated }) => { refresh(updated); navigate("/admin/users?view=deleted"); }, onError: (cause) => setError(cause.message) });
  const restore = useMutation({ mutationFn: () => api.restoreAdminUser(user.id), onSuccess: ({ user: updated }) => { refresh(updated); navigate("/admin/users"); }, onError: (cause) => setError(cause.message) });
  const purge = useMutation({ mutationFn: () => api.purgeAdminUser(user.id), onSuccess: () => { void queryClient.invalidateQueries({ queryKey: queryKeys.adminUsers }); navigate("/admin/users?view=deleted"); }, onError: (cause) => setError(cause.message) });
  const pending = disable.isPending || enable.isPending || remove.isPending || restore.isPending || purge.isPending;
  const displayName = user.displayName || user.usernameNormalized;

  function confirmDisable() {
    if (pending || !window.confirm(`禁用“${displayName}”后会撤销登录凭证并停止播放，账号资料仍会保留。确定继续吗？`)) return;
    setError("");
    disable.mutate();
  }

  function confirmEnable() {
    if (pending || !window.confirm(`启用“${displayName}”后，用户可以重新登录。确定继续吗？`)) return;
    setError("");
    enable.mutate();
  }

  function confirmDelete() {
    if (pending || !window.confirm(`将“${displayName}”移入回收站会撤销凭证并停止播放，账号默认保留 7 天，可在期限内恢复。确定继续吗？`)) return;
    setError("");
    remove.mutate();
  }

  function confirmPurge() {
    if (pending || !window.confirm(`彻底删除“${displayName}”将永久删除账号、头像、权限设置、偏好、播放记录和设备关联，媒体文件不受影响，且无法恢复。确定继续吗？`)) return;
    setError("");
    purge.mutate();
  }

  return (
    <section className="lux-admin-panel lux-admin-user-lifecycle-panel" aria-labelledby="lux-admin-user-lifecycle-title">
      <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-lifecycle-title">账号状态</h2></div><span className={user.deletedAt != null || user.isDisabled ? "lux-user-badge is-warn" : "lux-user-badge is-ok"}>{user.deletedAt != null ? "回收站" : user.isDisabled ? "已禁用" : "正常"}</span></div>
      {user.deletedAt != null ? <p className="lux-admin-muted">删除于 {user.deletedAt ? new Date(user.deletedAt * 1000).toLocaleString() : "未知"} · 计划清理 {user.deletionPurgeAt ? new Date(user.deletionPurgeAt * 1000).toLocaleString() : "未知"}</p> : null}
      <div className="lux-admin-user-profile-actions">
        {user.deletedAt != null ? <>
          <button className="lux-button lux-button-secondary" type="button" onClick={() => restore.mutate()} disabled={pending}>{restore.isPending ? "恢复中…" : "恢复账号"}</button>
          <button className="lux-button lux-button-secondary lux-admin-purge-user" type="button" onClick={confirmPurge} disabled={pending}>{purge.isPending ? "删除中…" : "彻底删除"}</button>
        </> : <>
          {user.isDisabled
            ? <button className="lux-button lux-button-secondary" type="button" onClick={confirmEnable} disabled={pending}>{enable.isPending ? "启用中…" : "启用账号"}</button>
            : <button className="lux-button lux-button-secondary" type="button" onClick={confirmDisable} disabled={pending}>{disable.isPending ? "禁用中…" : "禁用账号"}</button>}
          <button className="lux-button lux-button-secondary lux-admin-purge-user" type="button" onClick={confirmDelete} disabled={pending}>{remove.isPending ? "移入回收站…" : "移入回收站"}</button>
        </>}
        {error ? <span role="alert">操作失败：{error}</span> : null}
      </div>
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
      {section.id === "retention" ? <AdminUserRetentionSettings /> : (
        <section className="lux-admin-user-module-placeholder" aria-labelledby="lux-admin-user-settings-title">
          <span className="lux-admin-user-module-icon"><SlidersHorizontal size={19} /></span>
          <div><span className="lux-admin-eyebrow">{section.module}</span><h2 id="lux-admin-user-settings-title">{section.label}</h2><p>全局设置结构已预留，配置和保存能力将在对应任务中实现。</p></div>
        </section>
      )}
    </div>
  );
}

function AdminUserRetentionSettings() {
  const queryClient = useQueryClient();
  const settings = useQuery({ queryKey: queryKeys.adminSettings, queryFn: () => api.adminSettings() });
  const [graceDays, setGraceDays] = useState("7");
  const [notice, setNotice] = useState("");
  const update = useMutation({
    mutationFn: (days: number) => api.updateAdminSettings({ userDeletionGraceDays: days }),
    onSuccess: (updated, days) => {
      const savedDays = updated.userDeletionGraceDays ?? days;
      queryClient.setQueryData(queryKeys.adminSettings, { ...updated, userDeletionGraceDays: savedDays });
      setGraceDays(String(savedDays));
      setNotice("删除宽限期已保存。");
    },
    onError: (error) => setNotice(error.message || "保存失败，请重试。"),
  });
  useEffect(() => {
    setGraceDays(String(settings.data?.userDeletionGraceDays ?? 7));
  }, [settings.data?.userDeletionGraceDays]);

  if (settings.isPending) return <p className="lux-admin-muted" role="status">正在读取删除保留设置…</p>;
  if (settings.error) return <p className="lux-error-copy" role="alert">读取删除保留设置失败：{settings.error.message}</p>;

  const parsedDays = Number(graceDays);
  const validDays = Number.isInteger(parsedDays) && parsedDays >= 1 && parsedDays <= 3_650;
  const hasChanges = parsedDays !== (settings.data.userDeletionGraceDays ?? 7);

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!validDays || !hasChanges || update.isPending) return;
    setNotice("");
    update.mutate(parsedDays);
  }

  return (
    <>
      <section className="lux-admin-panel lux-admin-user-profile-panel" aria-labelledby="lux-admin-user-retention-title">
        <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-retention-title">删除账号保留期限</h2></div></div>
        <form className="lux-admin-form lux-admin-user-profile-form" onSubmit={submit}>
          <label>
            移入回收站后保留天数
            <input
              type="number"
              min={1}
              max={3_650}
              step={1}
              value={graceDays}
              aria-label="删除账号保留天数"
              onChange={(event) => { setGraceDays(event.target.value); setNotice(""); }}
              disabled={update.isPending}
            />
          </label>
          <p className="lux-admin-muted">范围为 1–3650 天。新值只影响之后移入回收站的账号；已在回收站中的账号仍按原计划清理时间处理。</p>
          <div className="lux-admin-user-profile-actions">
            <button className="lux-button lux-button-primary" type="submit" disabled={!validDays || !hasChanges || update.isPending}>
              {update.isPending ? "保存中…" : "保存保留期限"}
            </button>
            {notice ? <span role={update.isError ? "alert" : "status"}>{notice}</span> : null}
          </div>
        </form>
      </section>
      <section className="lux-admin-user-module-placeholder" aria-labelledby="lux-admin-user-activity-retention-title">
        <span className="lux-admin-user-module-icon"><SlidersHorizontal size={19} /></span>
        <div><span className="lux-admin-eyebrow">模块七</span><h2 id="lux-admin-user-activity-retention-title">活动记录保留</h2><p>活动记录保留将在模块七接入。</p></div>
      </section>
    </>
  );
}

function AdminUserManagementState({ label, error = false }: { label: string; error?: boolean }) {
  return <section className="lux-admin-page-state" role={error ? "alert" : "status"}><h1>{error ? "无法打开用户详情" : "正在加载用户详情"}</h1><p>{label}</p><Link className="lux-button lux-button-secondary" to="/admin/users">返回用户列表</Link></section>;
}
