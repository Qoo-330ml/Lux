import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Check, KeyRound, Plus, ShieldCheck, Trash2, UserRound, X } from "lucide-react";
import { FormEvent, useState } from "react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminUser } from "../../lib/api/types";

export function AdminUsersPage() {
  const queryClient = useQueryClient();
  const users = useQuery({ queryKey: queryKeys.adminUsers, queryFn: () => api.adminUsers() });
  const libraries = useQuery({ queryKey: queryKeys.adminLibraries, queryFn: () => api.adminLibraries() });
  const [form, setForm] = useState({ username: "", displayName: "", password: "", isAdmin: false });
  const [formError, setFormError] = useState("");
  const create = useMutation({ mutationFn: () => api.createAdminUser(form), onSuccess: () => { setForm({ username: "", displayName: "", password: "", isAdmin: false }); setFormError(""); void queryClient.invalidateQueries({ queryKey: queryKeys.adminUsers }); }, onError: (error) => setFormError(error.message) });

  if (users.isPending || libraries.isPending) return <AdminUserState label="正在读取用户与权限…" />;
  if (users.error || libraries.error) return <AdminUserState label={users.error?.message || libraries.error?.message || "用户数据加载失败"} error />;
  const items = users.data.users ?? [];
  const availableLibraries = libraries.data.libraries ?? [];
  function submit(event: FormEvent) { event.preventDefault(); if (!form.username.trim() || !form.password) { setFormError("用户名和密码不能为空"); return; } create.mutate(); }

  return (
    <div className="lux-admin-page">
      <header className="lux-admin-page-heading"><div><h1>用户与权限</h1><p>管理账户状态、远程访问、下载和媒体库访问范围。</p></div></header>
      <section className="lux-admin-panel lux-admin-create-panel"><div className="lux-admin-panel-heading"><div><h2>创建用户</h2></div><Plus size={20} className="lux-admin-panel-icon" /></div><form className="lux-admin-form lux-admin-user-create-form" onSubmit={submit}><label>用户名<input value={form.username} onChange={(event) => setForm({ ...form, username: event.target.value })} autoComplete="off" /></label><label>显示名称<input value={form.displayName} onChange={(event) => setForm({ ...form, displayName: event.target.value })} /></label><label>初始密码<input type="password" value={form.password} onChange={(event) => setForm({ ...form, password: event.target.value })} autoComplete="new-password" /></label><label className="lux-admin-toggle"><input type="checkbox" checked={form.isAdmin} onChange={(event) => setForm({ ...form, isAdmin: event.target.checked })} /><span>服务器管理员</span></label><button className="lux-button lux-button-primary" type="submit" disabled={create.isPending}><Plus size={16} /> {create.isPending ? "创建中…" : "创建用户"}</button></form>{formError ? <p className="lux-error-copy">{formError}</p> : null}</section>
      <section className="lux-admin-panel"><div className="lux-admin-panel-heading"><div><h2>账户列表</h2></div><span className="lux-status-pill">{items.length} 个账户</span></div><div className="lux-admin-user-list">{items.map((user) => <UserAdminRow key={user.id} user={user} libraryIds={availableLibraries.map((library) => library.id)} libraryNames={new Map(availableLibraries.map((library) => [library.id, library.name]))} />)}</div></section>
    </div>
  );
}

function UserAdminRow({ user, libraryIds, libraryNames }: { user: AdminUser; libraryIds: string[]; libraryNames: Map<string, string> }) {
  const queryClient = useQueryClient();
  const [expanded, setExpanded] = useState(false);
  const [newPassword, setNewPassword] = useState("");
  const access = useQuery({ queryKey: ["admin", "user-access", user.id], queryFn: () => api.adminUserLibraryAccess(user.id), enabled: expanded });
  const update = useMutation({ mutationFn: (input: Record<string, unknown>) => api.updateAdminUser(user.id, input), onSuccess: () => { setNewPassword(""); void queryClient.invalidateQueries({ queryKey: queryKeys.adminUsers }); } });
  const disable = useMutation({ mutationFn: () => api.disableAdminUser(user.id), onSuccess: () => void queryClient.invalidateQueries({ queryKey: queryKeys.adminUsers }) });
  const setAccess = useMutation({ mutationFn: ({ libraryId, canView }: { libraryId: string; canView: boolean }) => api.setAdminUserLibraryAccess(user.id, libraryId, canView), onSuccess: () => void queryClient.invalidateQueries({ queryKey: ["admin", "user-access", user.id] }) });
  const enabledLibraryIds = new Set(access.data?.libraryIds ?? []);
  const displayName = user.displayName || user.usernameNormalized;

  function confirmDisable() {
    if (disable.isPending || !window.confirm(`确定要删除账户“${displayName}”吗？删除后账户将被禁用，无法登录。`)) return;
    disable.mutate();
  }

  return <article className={user.isDisabled ? "lux-admin-user-row is-disabled" : "lux-admin-user-row"}><div className="lux-admin-user-summary"><span className="lux-admin-user-avatar"><UserRound size={18} /></span><div><strong>{displayName}</strong><small>{user.usernameNormalized}{user.canManageServer ? " · 管理员" : ""}</small></div></div><div className="lux-admin-user-badges"><span className={user.isDisabled ? "lux-user-badge is-warn" : "lux-user-badge is-ok"}>{user.isDisabled ? "已禁用" : "正常"}</span><span className="lux-user-badge">{user.canRemoteAccess ? "远程" : "仅本地"}</span></div><div className="lux-admin-user-actions"><button className="lux-button lux-button-compact lux-button-secondary" type="button" onClick={() => setExpanded((value) => !value)}>{expanded ? <X size={15} /> : <ShieldCheck size={15} />} {expanded ? "收起" : "管理权限"}</button><button className="lux-icon-button lux-icon-button-small" type="button" aria-label={`保存 ${displayName} 权限`} onClick={() => update.mutate({ canManageServer: !user.canManageServer })} title="切换服务器管理权限"><ShieldCheck size={15} /></button><button className="lux-icon-button lux-icon-button-small lux-danger-icon lux-admin-delete-user" type="button" aria-label={`删除${displayName}账户`} title="删除账户（禁用登录）" onClick={confirmDisable} disabled={disable.isPending}><Trash2 size={15} /></button></div>{disable.error ? <p className="lux-error-copy lux-admin-user-action-error" role="alert">删除失败：{disable.error.message}</p> : null}{expanded ? <div className="lux-admin-user-detail"><div className="lux-admin-permission-grid"><PermissionToggle label="服务器管理" checked={user.canManageServer} onChange={(checked) => update.mutate({ canManageServer: checked })} /><PermissionToggle label="远程访问" checked={user.canRemoteAccess} onChange={(checked) => update.mutate({ canRemoteAccess: checked })} /><PermissionToggle label="允许下载" checked={user.canDownload} onChange={(checked) => update.mutate({ canDownload: checked })} /><PermissionToggle label="账户启用" checked={!user.isDisabled} onChange={(checked) => update.mutate({ isDisabled: !checked })} /></div><div className="lux-admin-access-block"><div className="lux-admin-subpanel-heading"><strong>媒体库访问</strong><span>选择用户可见的库</span></div>{access.isPending ? <p className="lux-admin-muted">正在读取…</p> : libraryIds.length === 0 ? <p className="lux-admin-muted">暂无媒体库。</p> : <div className="lux-admin-library-access">{libraryIds.map((libraryId) => <label key={libraryId}><input type="checkbox" checked={enabledLibraryIds.has(libraryId)} onChange={(event) => setAccess.mutate({ libraryId, canView: event.target.checked })} /><span>{libraryNames.get(libraryId) ?? libraryId}</span></label>)}</div>}</div><div className="lux-admin-password-row"><label>设置新密码<input type="password" value={newPassword} onChange={(event) => setNewPassword(event.target.value)} autoComplete="new-password" placeholder="留空不修改" /></label><button className="lux-button lux-button-secondary" type="button" disabled={!newPassword || update.isPending} onClick={() => update.mutate({ password: newPassword })}><KeyRound size={15} /> 修改密码</button></div>{update.error ? <p className="lux-error-copy">{update.error.message}</p> : null}</div> : null}</article>;
}

function PermissionToggle({ label, checked, onChange }: { label: string; checked?: boolean; onChange: (checked: boolean) => void }) { return <label className="lux-admin-permission-toggle"><input type="checkbox" checked={Boolean(checked)} onChange={(event) => onChange(event.target.checked)} /><span>{checked ? <Check size={13} /> : null}</span>{label}</label>; }
function AdminUserState({ label, error = false }: { label: string; error?: boolean }) { return <section className="lux-admin-page-state" role={error ? "alert" : "status"}><h1>{error ? "用户数据加载失败" : "正在加载用户"}</h1><p>{label}</p></section>; }
