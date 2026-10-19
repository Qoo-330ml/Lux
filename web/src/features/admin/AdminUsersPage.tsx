import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowDown, ArrowUp, Ban, Check, KeyRound, Plus, RotateCcw, Search, ShieldCheck, Trash2, UserCheck, UserRound, X } from "lucide-react";
import { FormEvent, useEffect, useRef, useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminUser, AdminUserListStatus, AdminUserPermissionPresetBatchErrorCode, AdminUserSortBy, AdminUserSortOrder, AdminUserStatusBatchErrorCode } from "../../lib/api/types";
import { AdminUserPermissionCopyDialog } from "./AdminUserPermissionCopyDialog";
import { AdminUserPreferenceCopyDialog } from "./AdminUserPreferenceCopyDialog";
import "./user-management.css";

export function AdminUsersPage() {
  const queryClient = useQueryClient();
  const [searchParams, setSearchParams] = useSearchParams();
  const requestedView = searchParams.get("view");
  const view = requestedView === "pending" || requestedView === "deleted" ? requestedView : "users";
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = useState(20);
  const [searchInput, setSearchInput] = useState("");
  const [search, setSearch] = useState("");
  const [status, setStatus] = useState<AdminUserListStatus>("all");
  const [sortBy, setSortBy] = useState<AdminUserSortBy>("lastActivityAt");
  const [sortOrder, setSortOrder] = useState<AdminUserSortOrder>("desc");
  useEffect(() => {
    const timer = window.setTimeout(() => setSearch(searchInput.trim()), 250);
    return () => window.clearTimeout(timer);
  }, [searchInput]);
  useEffect(() => setPage(1), [view, pageSize, search, status, sortBy, sortOrder]);
  const queryStatus: AdminUserListStatus = view === "deleted" ? "deleted" : status;
  const users = useQuery({
    queryKey: queryKeys.adminUsersPage(page, pageSize, search, queryStatus, sortBy, sortOrder),
    queryFn: () => api.adminUsers({ page, pageSize, search, status: queryStatus, sortBy, sortOrder }),
    enabled: view !== "pending",
  });
  const settings = useQuery({ queryKey: queryKeys.adminSettings, queryFn: () => api.adminSettings() });
  const [selectedUserIds, setSelectedUserIds] = useState<string[]>([]);
  const permissionPresets = useQuery({
    queryKey: ["admin", "user-permission-presets"],
    queryFn: () => api.adminUserPermissionPresets(),
    enabled: view === "users" && selectedUserIds.length > 0,
  });
  const deletionGraceDays = settings.data?.userDeletionGraceDays;
  const [form, setForm] = useState({ username: "", displayName: "", password: "", isAdmin: false });
  const [formError, setFormError] = useState("");
  const [batchPresetId, setBatchPresetId] = useState("");
  const [batchResults, setBatchResults] = useState<{ presetName: string; results: Array<{ userId: string; userName: string; success: boolean; errorCode: AdminUserPermissionPresetBatchErrorCode | null }> } | null>(null);
  const [batchError, setBatchError] = useState("");
  const [batchStatusResults, setBatchStatusResults] = useState<{ isDisabled: boolean; results: Array<{ userId: string; userName: string; success: boolean; errorCode: AdminUserStatusBatchErrorCode | null }> } | null>(null);
  const [batchStatusError, setBatchStatusError] = useState("");
  const [copyDialogOpen, setCopyDialogOpen] = useState(false);
  const [permissionCopyDialogOpen, setPermissionCopyDialogOpen] = useState(false);
  const anyCopyDialogOpen = copyDialogOpen || permissionCopyDialogOpen;
  const permissionCopyButtonRef = useRef<HTMLButtonElement>(null);
  const batchPanelHeadingRef = useRef<HTMLHeadingElement>(null);
  const items = users.data?.users ?? [];
  const totalCount = users.data?.totalCount ?? 0;
  const totalPages = Math.max(1, Math.ceil(totalCount / pageSize));
  const selectedOnCurrentPage = items.filter((user) => selectedUserIds.includes(user.id));
  const allCurrentPageSelected = items.length > 0 && selectedOnCurrentPage.length === items.length;
  const create = useMutation({ mutationFn: () => api.createAdminUser(form), onSuccess: () => { setForm({ username: "", displayName: "", password: "", isAdmin: false }); setFormError(""); void queryClient.invalidateQueries({ queryKey: queryKeys.adminUsers }); }, onError: (error) => setFormError(error.message) });
  const applyBatchPreset = useMutation({
    mutationFn: ({ userIds, presetId }: { userIds: string[]; presetId: string; presetName: string; userNames: Record<string, string> }) => api.applyAdminUserPermissionPresetBatch(userIds, presetId),
    onSuccess: (response, variables) => {
      setBatchResults({
        presetName: variables.presetName,
        results: response.results.map((result) => ({
          ...result,
          userName: variables.userNames[result.userId] ?? result.userId,
        })),
      });
      setBatchError("");
      setSelectedUserIds([]);
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminUsers });
    },
    onError: (error) => setBatchError(error.message),
  });
  const updateBatchStatus = useMutation({
    mutationFn: ({ userIds, isDisabled }: { userIds: string[]; isDisabled: boolean; userNames: Record<string, string> }) => api.updateAdminUserStatusBatch(userIds, isDisabled),
    onSuccess: (response, variables) => {
      setBatchStatusResults({
        isDisabled: variables.isDisabled,
        results: response.results.map((result) => ({
          ...result,
          userName: variables.userNames[result.userId] ?? result.userId,
        })),
      });
      setBatchStatusError("");
      setSelectedUserIds([]);
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminUsers });
    },
    onError: (error) => setBatchStatusError(error.message),
  });
  const batchActionPending = applyBatchPreset.isPending || updateBatchStatus.isPending;
  useEffect(() => {
    setSelectedUserIds([]);
    setBatchResults(null);
    setBatchError("");
    setBatchStatusResults(null);
    setBatchStatusError("");
  }, [view, page, pageSize, searchInput, status, sortBy, sortOrder]);

  if (view !== "pending" && users.error && !users.data) return <AdminUserState label={users.error.message || "用户数据加载失败"} error />;
  if (view !== "pending" && users.isPending) return <AdminUserState label="正在读取用户列表…" />;
  function submit(event: FormEvent) { event.preventDefault(); if (!form.username.trim() || !form.password) { setFormError("用户名和密码不能为空"); return; } create.mutate(); }

  function toggleUserSelection(userId: string, selected: boolean) {
    setSelectedUserIds((current) => selected
      ? current.includes(userId) ? current : [...current, userId]
      : current.filter((id) => id !== userId));
    setBatchError("");
    setBatchStatusError("");
    setBatchResults(null);
    setBatchStatusResults(null);
  }

  function toggleCurrentPageSelection() {
    if (allCurrentPageSelected) {
      const pageIds = new Set(items.map((user) => user.id));
      setSelectedUserIds((current) => current.filter((id) => !pageIds.has(id)));
    } else {
      setSelectedUserIds((current) => [...new Set([...current, ...items.map((user) => user.id)])]);
    }
    setBatchError("");
    setBatchStatusError("");
    setBatchResults(null);
    setBatchStatusResults(null);
  }

  function confirmBatchPreset() {
    const preset = permissionPresets.data?.presets.find((candidate) => candidate.id === batchPresetId);
    const targets = selectedOnCurrentPage;
    if (!preset || targets.length === 0 || applyBatchPreset.isPending || anyCopyDialogOpen) return;
    const names = targets.map((user) => user.displayName || user.usernameNormalized).join("、");
    if (!window.confirm(`仅对当前页已选的 ${targets.length} 个用户应用权限预设“${preset.name}”吗？\n${names}`)) return;
    setBatchResults(null);
    setBatchError("");
    setBatchStatusResults(null);
    setBatchStatusError("");
    applyBatchPreset.mutate({
      userIds: targets.map((user) => user.id),
      presetId: preset.id,
      presetName: preset.name,
      userNames: Object.fromEntries(targets.map((user) => [user.id, user.displayName || user.usernameNormalized])),
    });
  }

  function confirmBatchUserStatus(isDisabled: boolean) {
    const targets = selectedOnCurrentPage;
    if (targets.length === 0 || batchActionPending || anyCopyDialogOpen) return;
    const names = targets.map((user) => user.displayName || user.usernameNormalized).join("、");
    const action = isDisabled ? "禁用" : "启用";
    const effect = isDisabled
      ? "禁用会撤销登录凭证并停止服务端控制的播放；已交给外部播放器的 STRM 链接无法强制中断。"
      : "启用后，用户需要重新登录；已撤销的旧登录不会恢复。";
    if (!window.confirm(`仅对当前页已选的 ${targets.length} 个用户${action}账号吗？\n${names}\n${effect}`)) return;
    setBatchStatusResults(null);
    setBatchStatusError("");
    setBatchResults(null);
    setBatchError("");
    updateBatchStatus.mutate({
      userIds: targets.map((user) => user.id),
      isDisabled,
      userNames: Object.fromEntries(targets.map((user) => [user.id, user.displayName || user.usernameNormalized])),
    });
  }

  function closePermissionCopyDialog() {
    setPermissionCopyDialogOpen(false);
    window.requestAnimationFrame(() => {
      if (permissionCopyButtonRef.current?.disabled) batchPanelHeadingRef.current?.focus();
      else permissionCopyButtonRef.current?.focus();
    });
  }

  return (
    <div className="lux-admin-page">
      <header className="lux-admin-users-heading"><div><span className="lux-admin-eyebrow">账户与访问</span><h1>用户管理</h1><p>管理账户资料、状态和服务访问范围。</p></div><Link className="lux-button lux-button-secondary" to="/admin/users/settings">全局设置</Link></header>
      <nav className="lux-admin-users-nav" aria-label="用户管理视图">
        {[
          ["users", "用户列表"],
          ["pending", "待审批"],
          ["deleted", "回收站"],
        ].map(([key, label]) => (
          <button key={key} className={view === key ? "is-active" : undefined} type="button" aria-current={view === key ? "page" : undefined} onClick={() => { setPage(1); setSearchParams(key === "users" ? {} : { view: key }); }}>{label}</button>
        ))}
      </nav>
      {view === "pending" ? (
        <section className="lux-admin-users-placeholder" aria-labelledby="lux-admin-users-placeholder-title">
          <span className="lux-admin-eyebrow">后续任务</span>
          <h2 id="lux-admin-users-placeholder-title">待审批用户</h2>
          <p>注册申请和审批将在注册邀请任务中启用。</p>
        </section>
      ) : <>
      <div className="lux-admin-user-query-controls">
        <label className="lux-admin-user-search"><span>搜索用户</span><div><Search size={16} aria-hidden="true" /><input value={searchInput} onChange={(event) => setSearchInput(event.target.value)} placeholder="用户名、显示名称或邮箱" /></div></label>
        {view === "users" ? <label><span>账号状态</span><select value={status} onChange={(event) => setStatus(event.target.value as AdminUserListStatus)}><option value="all">全部状态</option><option value="active">正常</option><option value="disabled">已禁用</option></select></label> : <div className="lux-admin-deleted-hint"><span>回收站</span><small>账号将在显示的期限后自动清理</small></div>}
        <label><span>排序方式</span><select value={sortBy} onChange={(event) => setSortBy(event.target.value as AdminUserSortBy)}><option value="lastActivityAt">最近活动</option><option value="lastLoginAt">最近登录</option><option value="createdAt">创建时间</option></select></label>
        <button className="lux-button lux-button-compact lux-button-secondary lux-admin-user-sort-order" type="button" onClick={() => setSortOrder((order) => order === "desc" ? "asc" : "desc")} aria-label={sortOrder === "desc" ? "当前最新优先，切换为最早优先" : "当前最早优先，切换为最新优先"} title={sortOrder === "desc" ? "最新优先" : "最早优先"}>{sortOrder === "desc" ? <ArrowDown size={16} /> : <ArrowUp size={16} />}{sortOrder === "desc" ? "最新优先" : "最早优先"}</button>
        <label><span>每页</span><select value={pageSize} onChange={(event) => setPageSize(Number(event.target.value))}><option value={20}>20 条</option><option value={50}>50 条</option><option value={100}>100 条</option></select></label>
      </div>
      {view === "users" ? <>
        <section className="lux-admin-panel lux-admin-create-panel"><div className="lux-admin-panel-heading"><div><h2>创建用户</h2></div><Plus size={20} className="lux-admin-panel-icon" /></div><form className="lux-admin-form lux-admin-user-create-form" onSubmit={submit}><label>用户名<input value={form.username} onChange={(event) => setForm({ ...form, username: event.target.value })} autoComplete="off" /></label><label>显示名称<input value={form.displayName} onChange={(event) => setForm({ ...form, displayName: event.target.value })} /></label><label>初始密码<input type="password" value={form.password} onChange={(event) => setForm({ ...form, password: event.target.value })} autoComplete="new-password" /></label><label className="lux-admin-toggle"><input type="checkbox" checked={form.isAdmin} onChange={(event) => setForm({ ...form, isAdmin: event.target.checked })} /><span>服务器管理员</span></label><button className="lux-button lux-button-primary" type="submit" disabled={create.isPending}><Plus size={16} /> {create.isPending ? "创建中…" : "创建用户"}</button></form>{formError ? <p className="lux-error-copy">{formError}</p> : null}</section>
        <section className="lux-admin-panel lux-admin-user-batch-panel" aria-labelledby="lux-admin-user-batch-title">
          <div className="lux-admin-panel-heading"><div><h2 ref={batchPanelHeadingRef} id="lux-admin-user-batch-title" tabIndex={-1}>批量用户操作</h2><p>仅对当前页勾选的用户操作；换页或筛选后会清空选择，每批最多 100 人。</p></div></div>
          <div className="lux-admin-user-batch-controls">
            <button className="lux-button lux-button-compact lux-button-secondary" type="button" aria-label={allCurrentPageSelected ? "取消本页全选" : "全选本页"} onClick={toggleCurrentPageSelection} disabled={!items.length || batchActionPending || anyCopyDialogOpen}>{allCurrentPageSelected ? "取消本页全选" : "全选本页"}</button>
            <span aria-live="polite">当前页已选 {selectedOnCurrentPage.length} 人</span>
            <button className="lux-button lux-button-compact lux-button-secondary" type="button" aria-label="批量启用用户" onClick={() => confirmBatchUserStatus(false)} disabled={!selectedOnCurrentPage.length || batchActionPending || anyCopyDialogOpen}>{updateBatchStatus.isPending && updateBatchStatus.variables?.isDisabled === false ? "启用中…" : "批量启用"}</button>
            <button className="lux-button lux-button-compact lux-button-secondary" type="button" aria-label="批量禁用用户" onClick={() => confirmBatchUserStatus(true)} disabled={!selectedOnCurrentPage.length || batchActionPending || anyCopyDialogOpen}>{updateBatchStatus.isPending && updateBatchStatus.variables?.isDisabled === true ? "禁用中…" : "批量禁用"}</button>
            <button className="lux-button lux-button-compact lux-button-secondary" type="button" aria-label="复制偏好" onClick={() => setCopyDialogOpen(true)} disabled={!selectedOnCurrentPage.length || batchActionPending || anyCopyDialogOpen}>复制偏好</button>
            <button ref={permissionCopyButtonRef} className="lux-button lux-button-compact lux-button-secondary" type="button" aria-label="复制权限" onClick={() => setPermissionCopyDialogOpen(true)} disabled={!selectedOnCurrentPage.length || batchActionPending || anyCopyDialogOpen}>复制权限</button>
            <label>权限预设<select aria-label="批量权限预设" value={batchPresetId} onChange={(event) => setBatchPresetId(event.target.value)} disabled={permissionPresets.isPending || batchActionPending || anyCopyDialogOpen}>
              <option value="">选择权限预设</option>
              {(permissionPresets.data?.presets ?? []).map((preset) => <option key={preset.id} value={preset.id}>{preset.name}</option>)}
            </select></label>
            <button className="lux-button lux-button-primary lux-button-compact" type="button" aria-label="批量应用权限预设" onClick={confirmBatchPreset} disabled={!selectedOnCurrentPage.length || !batchPresetId || !permissionPresets.data?.presets.some((preset) => preset.id === batchPresetId) || batchActionPending || anyCopyDialogOpen}>{applyBatchPreset.isPending ? "应用中…" : "应用预设"}</button>
          </div>
          {selectedUserIds.length > 0 && permissionPresets.error ? <p className="lux-error-copy" role="alert">权限预设加载失败：{permissionPresets.error.message}</p> : null}
          {batchError ? <p className="lux-error-copy" role="alert">批量应用失败：{batchError}</p> : null}
          {batchStatusError ? <p className="lux-error-copy" role="alert">批量状态更新失败：{batchStatusError}</p> : null}
          {batchResults ? <div className="lux-admin-user-batch-results" role="status" aria-label="批量应用结果"><strong>权限预设“{batchResults.presetName}”的应用结果</strong><ul>{batchResults.results.map((result) => <li key={result.userId} className={result.success ? "is-success" : "is-failure"}>{result.userName}：{result.success ? "成功" : batchPermissionPresetErrorMessage(result.errorCode)}</li>)}</ul></div> : null}
          {batchStatusResults ? <div className="lux-admin-user-batch-results" role="status" aria-label="批量账号状态结果"><strong>批量{batchStatusResults.isDisabled ? "禁用" : "启用"}结果</strong><ul>{batchStatusResults.results.map((result) => <li key={result.userId} className={result.success ? "is-success" : "is-failure"}>{result.userName}：{result.success ? "成功" : batchUserStatusErrorMessage(result.errorCode)}</li>)}</ul></div> : null}
        </section>
        {copyDialogOpen ? <AdminUserPreferenceCopyDialog
          targets={selectedOnCurrentPage.map((user) => ({ id: user.id, name: user.displayName || user.usernameNormalized }))}
          onClose={() => setCopyDialogOpen(false)}
          onApplied={() => {
            setSelectedUserIds([]);
            void queryClient.invalidateQueries({ queryKey: queryKeys.adminUsers });
          }}
        /> : null}
        {permissionCopyDialogOpen ? <AdminUserPermissionCopyDialog
          targets={selectedOnCurrentPage.map((user) => ({ id: user.id, name: user.displayName || user.usernameNormalized }))}
          onClose={closePermissionCopyDialog}
          onApplied={() => {
            setSelectedUserIds([]);
            void queryClient.invalidateQueries({ queryKey: queryKeys.adminUsers });
          }}
        /> : null}
      </> : null}
      <section className="lux-admin-panel lux-admin-users-list-panel"><div className="lux-admin-panel-heading"><div><h2>{view === "deleted" ? "已删除用户" : "账户列表"}</h2></div><span className="lux-status-pill">共 {totalCount} 个账户</span></div>
        {users.error ? <p className="lux-error-copy" role="alert">列表更新失败：{users.error.message}</p> : null}
        {items.length ? <div className="lux-admin-user-list">{items.map((user) => view === "deleted"
          ? <DeletedUserRow key={user.id} user={user} />
          : <UserAdminRow key={user.id} user={user} graceDays={deletionGraceDays} selected={selectedUserIds.includes(user.id)} selectionDisabled={batchActionPending || anyCopyDialogOpen} actionsDisabled={batchActionPending || anyCopyDialogOpen} onSelectionChange={(selected) => toggleUserSelection(user.id, selected)} />)}</div> : <p className="lux-admin-muted lux-admin-users-empty">{view === "deleted" ? "回收站为空。" : search ? "没有找到匹配的用户。" : "当前没有用户。"}</p>}
        <div className="lux-admin-user-pagination" aria-label="用户列表分页"><span>{totalCount ? `显示 ${(page - 1) * pageSize + 1}–${Math.min(page * pageSize, totalCount)} 条，共 ${totalCount} 条` : "共 0 条"}</span><div><button className="lux-button lux-button-compact lux-button-secondary" type="button" disabled={page <= 1 || users.isFetching} onClick={() => setPage((current) => Math.max(1, current - 1))}>上一页</button><span>第 {page} / {totalPages} 页</span><button className="lux-button lux-button-compact lux-button-secondary" type="button" disabled={page >= totalPages || users.isFetching} onClick={() => setPage((current) => Math.min(totalPages, current + 1))}>下一页</button></div></div>
      </section>
      </>}
    </div>
  );
}

function UserAdminRow({ user, graceDays, selected, selectionDisabled, actionsDisabled, onSelectionChange }: { user: AdminUser; graceDays?: number; selected: boolean; selectionDisabled: boolean; actionsDisabled: boolean; onSelectionChange: (selected: boolean) => void }) {
  const queryClient = useQueryClient();
  const [expanded, setExpanded] = useState(false);
  const [newPassword, setNewPassword] = useState("");
  const update = useMutation({ mutationFn: (input: Record<string, unknown>) => api.updateAdminUser(user.id, input), onSuccess: () => { setNewPassword(""); void queryClient.invalidateQueries({ queryKey: queryKeys.adminUsers }); } });
  const disable = useMutation({ mutationFn: () => api.disableAdminUser(user.id), onSuccess: () => void queryClient.invalidateQueries({ queryKey: queryKeys.adminUsers }) });
  const enable = useMutation({ mutationFn: () => api.updateAdminUser(user.id, { isDisabled: false }), onSuccess: () => void queryClient.invalidateQueries({ queryKey: queryKeys.adminUsers }) });
  const remove = useMutation({ mutationFn: () => api.deleteAdminUser(user.id), onSuccess: () => void queryClient.invalidateQueries({ queryKey: queryKeys.adminUsers }) });
  const displayName = user.displayName || user.usernameNormalized;
  const actionPending = disable.isPending || enable.isPending || remove.isPending || actionsDisabled;

  function confirmDisable() {
    if (actionPending || user.isDisabled || !window.confirm(`确定要禁用账户“${displayName}”吗？账户数据会保留，但用户将无法登录。`)) return;
    disable.mutate();
  }

  function confirmEnable() {
    if (actionPending || !user.isDisabled || !window.confirm(`确定要启用账户“${displayName}”吗？账户将恢复登录。`)) return;
    enable.mutate();
  }

  function confirmDelete() {
    const retention = graceDays === undefined ? "按服务器当前配置的期限保留" : `保留 ${graceDays} 天`;
    if (actionPending || !window.confirm(`确定将账户“${displayName}”移入回收站吗？账号会${retention}，你可以在期限内恢复。`)) return;
    remove.mutate();
  }

  return (
    <article className={user.isDisabled ? "lux-admin-user-row is-disabled" : "lux-admin-user-row"}>
      <div className="lux-admin-user-summary">
        <label className="lux-admin-user-select"><input type="checkbox" aria-label={`选择${user.displayName || user.usernameNormalized}`} checked={selected} disabled={selectionDisabled} onChange={(event) => onSelectionChange(event.target.checked)} /><span className="lux-visually-hidden">选择账户参与当前页批量操作</span></label>
        <AdminUserAvatar userId={user.id} />
        <div>
          <strong>{displayName}</strong>
          <small>{user.usernameNormalized}{user.canManageServer ? " · 管理员" : ""}{user.email ? ` · ${user.email}` : ""}</small>
          <small>预设来源：{user.presetSource || "未设置"}</small>
          <small>最近活动：{formatUserTimestamp(user.lastActivityAt)} · 最近登录：{formatUserTimestamp(user.lastLoginAt)}</small>
        </div>
      </div>
      <div className="lux-admin-user-badges">
        <span className={user.isDisabled ? "lux-user-badge is-warn" : "lux-user-badge is-ok"}>
          {user.isDisabled ? "已禁用" : "正常"}
        </span>
        <span className="lux-user-badge">{user.canRemoteAccess ? "远程" : "仅本地"}</span>
        <span className="lux-user-badge">播放中 {user.currentPlaybackCount ?? 0}</span>
      </div>
      <div className="lux-admin-user-actions">
        <Link className="lux-button lux-button-compact lux-button-secondary" to={`/admin/users/${encodeURIComponent(user.id)}`}>详情</Link>
        <button className="lux-button lux-button-compact lux-button-secondary" type="button" onClick={() => setExpanded((value) => !value)} disabled={actionPending}>
          {expanded ? <X size={15} /> : <ShieldCheck size={15} />} {expanded ? "收起" : "管理权限"}
        </button>
        <button
          className="lux-icon-button lux-icon-button-small"
          type="button"
          aria-label={`保存 ${displayName} 权限`}
          onClick={() => update.mutate({ canManageServer: !user.canManageServer })}
          title="切换服务器管理权限"
          disabled={actionPending}
        >
          <ShieldCheck size={15} />
        </button>
        {user.isDisabled ? (
          <button
            className="lux-button lux-button-compact lux-button-secondary lux-admin-enable-user"
            type="button"
            aria-label={`启用${displayName}账户`}
            title="启用账户"
            onClick={confirmEnable}
            disabled={actionPending}
          >
            <UserCheck size={15} /> 启用
          </button>
        ) : (
          <button
            className="lux-button lux-button-compact lux-button-secondary lux-admin-disable-user"
            type="button"
            aria-label={`禁用${displayName}账户`}
            title="禁用账户（保留数据）"
            onClick={confirmDisable}
            disabled={actionPending}
          >
            <Ban size={15} /> 禁用
          </button>
        )}
        <button
          className="lux-icon-button lux-icon-button-small lux-danger-icon lux-admin-delete-user"
          type="button"
          aria-label={`删除${displayName}账户`}
          title={graceDays === undefined ? "按当前服务端设置移入回收站" : `移入回收站，保留 ${graceDays} 天`}
          onClick={confirmDelete}
          disabled={actionPending}
        >
          <Trash2 size={15} />
        </button>
      </div>
      {disable.error ? <p className="lux-error-copy lux-admin-user-action-error" role="alert">禁用失败：{disable.error.message}</p> : null}
      {enable.error ? <p className="lux-error-copy lux-admin-user-action-error" role="alert">启用失败：{enable.error.message}</p> : null}
      {remove.error ? <p className="lux-error-copy lux-admin-user-action-error" role="alert">删除失败：{remove.error.message}</p> : null}
      {expanded ? (
        <div className="lux-admin-user-detail">
          <div className="lux-admin-permission-grid">
            <PermissionToggle label="服务器管理" checked={user.canManageServer} onChange={(checked) => update.mutate({ canManageServer: checked })} disabled={actionPending} />
            <PermissionToggle label="远程访问" checked={user.canRemoteAccess} onChange={(checked) => update.mutate({ canRemoteAccess: checked })} disabled={actionPending} />
            <PermissionToggle label="允许下载" checked={user.canDownload} onChange={(checked) => update.mutate({ canDownload: checked })} disabled={actionPending} />
            <PermissionToggle label="账户启用" checked={!user.isDisabled} onChange={(checked) => update.mutate({ isDisabled: !checked })} disabled={actionPending} />
          </div>
          <div className="lux-admin-access-block">
            <div className="lux-admin-subpanel-heading"><strong>媒体库访问</strong></div>
            <p className="lux-admin-muted">权限范围支持全部、指定和无访问。请在用户详情的权限模块中设置。</p>
            <Link className="lux-button lux-button-compact lux-button-secondary" to={`/admin/users/${encodeURIComponent(user.id)}?section=permissions`}>设置媒体库权限</Link>
          </div>
          <div className="lux-admin-password-row">
            <label>
              设置新密码
              <input
                type="password"
                value={newPassword}
                onChange={(event) => setNewPassword(event.target.value)}
                autoComplete="new-password"
                placeholder="留空不修改"
                disabled={actionPending}
              />
            </label>
            <button
              className="lux-button lux-button-secondary"
              type="button"
              disabled={!newPassword || update.isPending || actionPending}
              onClick={() => update.mutate({ password: newPassword })}
            >
              <KeyRound size={15} /> 修改密码
            </button>
          </div>
          {update.error ? <p className="lux-error-copy">{update.error.message}</p> : null}
        </div>
      ) : null}
    </article>
  );
}

function DeletedUserRow({ user }: { user: AdminUser }) {
  const queryClient = useQueryClient();
  const restore = useMutation({ mutationFn: () => api.restoreAdminUser(user.id), onSuccess: () => void queryClient.invalidateQueries({ queryKey: queryKeys.adminUsers }) });
  const purge = useMutation({ mutationFn: () => api.purgeAdminUser(user.id), onSuccess: () => void queryClient.invalidateQueries({ queryKey: queryKeys.adminUsers }) });
  const displayName = user.displayName || user.usernameNormalized;
  const actionPending = restore.isPending || purge.isPending;

  function confirmPurge() {
    if (actionPending || !window.confirm(`彻底删除“${displayName}”将永久移除账号、头像、权限设置、偏好、播放记录和设备关联，无法恢复。媒体文件不会删除。确定继续吗？`)) return;
    purge.mutate();
  }

  return (
    <article className="lux-admin-user-row lux-admin-deleted-user-row">
      <div className="lux-admin-user-summary">
        <AdminUserAvatar userId={user.id} />
        <div>
          <strong>{displayName}</strong>
          <small>{user.usernameNormalized}{user.email ? ` · ${user.email}` : ""}</small>
          <small>删除于 {formatUserTimestamp(user.deletedAt)} · 计划清理 {formatUserTimestamp(user.deletionPurgeAt)}</small>
        </div>
      </div>
      <div className="lux-admin-user-badges"><span className="lux-user-badge is-warn">回收站</span></div>
      <div className="lux-admin-user-actions">
        <Link className="lux-button lux-button-compact lux-button-secondary" to={`/admin/users/${encodeURIComponent(user.id)}`}>详情</Link>
        <button className="lux-button lux-button-compact lux-button-secondary" type="button" onClick={() => restore.mutate()} disabled={actionPending}><RotateCcw size={15} />{restore.isPending ? "恢复中…" : "恢复"}</button>
        <button className="lux-button lux-button-compact lux-button-secondary lux-admin-purge-user" type="button" onClick={confirmPurge} disabled={actionPending}><Trash2 size={15} />{purge.isPending ? "删除中…" : "彻底删除"}</button>
      </div>
      {restore.error ? <p className="lux-error-copy lux-admin-user-action-error" role="alert">恢复失败：{restore.error.message}</p> : null}
      {purge.error ? <p className="lux-error-copy lux-admin-user-action-error" role="alert">彻底删除失败：{purge.error.message}</p> : null}
    </article>
  );
}

function PermissionToggle({ label, checked, disabled, onChange }: { label: string; checked?: boolean; disabled?: boolean; onChange: (checked: boolean) => void }) { return <label className="lux-admin-permission-toggle"><input type="checkbox" checked={Boolean(checked)} disabled={disabled} onChange={(event) => onChange(event.target.checked)} /><span>{checked ? <Check size={13} /> : null}</span>{label}</label>; }
function AdminUserState({ label, error = false }: { label: string; error?: boolean }) { return <section className="lux-admin-page-state" role={error ? "alert" : "status"}><h1>{error ? "用户数据加载失败" : "正在加载用户"}</h1><p>{label}</p></section>; }

function batchPermissionPresetErrorMessage(errorCode: AdminUserPermissionPresetBatchErrorCode | null) {
  switch (errorCode) {
    case "INVALID_USER_ID": return "用户标识无效";
    case "USER_NOT_FOUND": return "用户不存在或已删除";
    case "LAST_MANAGER": return "最后一个管理员不能失去管理权限";
    case "INVALID_SNAPSHOT": return "预设权限快照无效";
    case "STORAGE_UNAVAILABLE": return "保存失败，请稍后重试";
    default: return "操作失败";
  }
}

function batchUserStatusErrorMessage(errorCode: AdminUserStatusBatchErrorCode | null) {
  switch (errorCode) {
    case "INVALID_USER_ID": return "用户标识无效";
    case "USER_NOT_FOUND": return "用户不存在或已删除";
    case "LAST_MANAGER": return "最后一个管理员不能失去管理权限";
    case "STORAGE_UNAVAILABLE": return "保存失败，请稍后重试";
    default: return "操作失败";
  }
}

function formatUserTimestamp(timestamp?: number | null): string {
  return timestamp ? new Date(timestamp * 1000).toLocaleString() : "暂无";
}

function AdminUserAvatar({ userId }: { userId: string }) {
  const [failed, setFailed] = useState(false);
  return (
    <span className="lux-admin-user-avatar">
      {failed ? <UserRound size={18} /> : <img src={api.adminUserAvatarUrl(userId)} alt="" loading="lazy" onError={() => setFailed(true)} />}
    </span>
  );
}
