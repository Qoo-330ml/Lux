import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowDown, ArrowUp, Ban, KeyRound, Plus, RotateCcw, Search, ShieldCheck, Trash2, UserCheck, X } from "lucide-react";
import { FormEvent, useEffect, useRef, useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminUser, AdminUserListStatus, AdminUserPermissionPresetBatchErrorCode, AdminUserSortBy, AdminUserSortOrder, AdminUserStatusBatchErrorCode } from "../../lib/api/types";
import { AdminUserPermissionCopyDialog } from "./AdminUserPermissionCopyDialog";
import { AdminUserPreferenceCopyDialog } from "./AdminUserPreferenceCopyDialog";
import { AdminUserInvitationsPanel } from "./AdminUserInvitationsPanel";
import { AdminUserRegistrationsPanel } from "./AdminUserRegistrationsPanel";
import { UsersWorkspace, type UsersViewKey } from "./users/UsersWorkspace";
import { formatRelativeTime, formatTimestamp } from "./users/format";

const VIEW_LABELS: Record<UsersViewKey, string> = {
  users: "全部用户",
  pending: "待审批",
  invitations: "邀请",
  deleted: "回收站",
};

export function AdminUsersPage() {
  const queryClient = useQueryClient();
  const [searchParams, setSearchParams] = useSearchParams();
  const requestedView = searchParams.get("view");
  const view: UsersViewKey = requestedView === "pending" || requestedView === "invitations" || requestedView === "deleted" ? requestedView : "users";
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = useState(20);
  const [searchInput, setSearchInput] = useState("");
  const [search, setSearch] = useState("");
  const [status, setStatus] = useState<AdminUserListStatus>("all");
  const [sortBy, setSortBy] = useState<AdminUserSortBy>("lastActivityAt");
  const [sortOrder, setSortOrder] = useState<AdminUserSortOrder>("desc");
  const [selectedUserIds, setSelectedUserIds] = useState<string[]>([]);
  const [openMenuId, setOpenMenuId] = useState<string | null>(null);
  const [actionError, setActionError] = useState("");
  const [createOpen, setCreateOpen] = useState(false);
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
  const createButtonRef = useRef<HTMLButtonElement>(null);
  const createFirstFieldRef = useRef<HTMLInputElement>(null);
  const batchPanelHeadingRef = useRef<HTMLHeadingElement>(null);
  const listHeadingRef = useRef<HTMLHeadingElement>(null);

  useEffect(() => {
    const timer = window.setTimeout(() => setSearch(searchInput.trim()), 250);
    return () => window.clearTimeout(timer);
  }, [searchInput]);

  useEffect(() => setPage(1), [view, pageSize, search, status, sortBy, sortOrder]);

  useEffect(() => {
    if (!openMenuId) return;
    function close(event: PointerEvent) {
      const target = event.target as HTMLElement | null;
      if (target?.closest("[data-user-menu]")) return;
      setOpenMenuId(null);
    }
    function closeOnEscape(event: KeyboardEvent) {
      if (event.key === "Escape") setOpenMenuId(null);
    }
    document.addEventListener("pointerdown", close);
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("pointerdown", close);
      document.removeEventListener("keydown", closeOnEscape);
    };
  }, [openMenuId]);

  useEffect(() => {
    if (!createOpen) return;
    createFirstFieldRef.current?.focus();
    function closeOnEscape(event: KeyboardEvent) {
      if (event.key === "Escape") closeCreate();
    }
    document.addEventListener("keydown", closeOnEscape);
    return () => document.removeEventListener("keydown", closeOnEscape);
  }, [createOpen]);

  useEffect(() => {
    setSelectedUserIds([]);
    setBatchResults(null);
    setBatchError("");
    setBatchStatusResults(null);
    setBatchStatusError("");
    setActionError("");
  }, [view, page, pageSize, searchInput, status, sortBy, sortOrder]);

  const queryStatus: AdminUserListStatus = view === "deleted" ? "deleted" : status;
  const users = useQuery({
    queryKey: queryKeys.adminUsersPage(page, pageSize, search, queryStatus, sortBy, sortOrder),
    queryFn: () => api.adminUsers({ page, pageSize, search, status: queryStatus, sortBy, sortOrder }),
    enabled: view === "users" || view === "deleted",
  });
  const settings = useQuery({ queryKey: queryKeys.adminSettings, queryFn: () => api.adminSettings(), enabled: view === "users" || view === "deleted" });
  const permissionPresets = useQuery({
    queryKey: ["admin", "user-permission-presets"],
    queryFn: () => api.adminUserPermissionPresets(),
    enabled: view === "users" && selectedUserIds.length > 0,
  });
  const deletionGraceDays = settings.data?.userDeletionGraceDays;
  const items = users.data?.users ?? [];
  const totalCount = users.data?.totalCount ?? 0;
  const totalPages = Math.max(1, Math.ceil(totalCount / pageSize));
  const selectedOnCurrentPage = items.filter((user) => selectedUserIds.includes(user.id));
  const allCurrentPageSelected = items.length > 0 && selectedOnCurrentPage.length === items.length;

  const invalidateUsers = () => queryClient.invalidateQueries({ queryKey: queryKeys.adminUsers });
  const create = useMutation({
    mutationFn: () => api.createAdminUser(form),
    onSuccess: () => {
      setForm({ username: "", displayName: "", password: "", isAdmin: false });
      setFormError("");
      setCreateOpen(false);
      createButtonRef.current?.focus();
      void invalidateUsers();
    },
    onError: (error) => setFormError(error.message),
  });
  const disableUser = useMutation({
    mutationFn: (userId: string) => api.disableAdminUser(userId),
    onSuccess: () => { setOpenMenuId(null); void invalidateUsers(); },
    onError: (error) => setActionError(`禁用失败：${error.message}`),
  });
  const enableUser = useMutation({
    mutationFn: (userId: string) => api.updateAdminUser(userId, { isDisabled: false }),
    onSuccess: () => { setOpenMenuId(null); void invalidateUsers(); },
    onError: (error) => setActionError(`启用失败：${error.message}`),
  });
  const deleteUser = useMutation({
    mutationFn: (userId: string) => api.deleteAdminUser(userId),
    onSuccess: () => { setOpenMenuId(null); void invalidateUsers(); },
    onError: (error) => setActionError(`删除失败：${error.message}`),
  });
  const restoreUser = useMutation({
    mutationFn: (userId: string) => api.restoreAdminUser(userId),
    onSuccess: () => { setOpenMenuId(null); void invalidateUsers(); },
    onError: (error) => setActionError(`恢复失败：${error.message}`),
  });
  const purgeUser = useMutation({
    mutationFn: (userId: string) => api.purgeAdminUser(userId),
    onSuccess: () => { setOpenMenuId(null); void invalidateUsers(); },
    onError: (error) => setActionError(`彻底删除失败：${error.message}`),
  });
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
      void invalidateUsers();
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
      void invalidateUsers();
    },
    onError: (error) => setBatchStatusError(error.message),
  });
  const batchActionPending = applyBatchPreset.isPending || updateBatchStatus.isPending;
  const rowActionPending = disableUser.isPending || enableUser.isPending || deleteUser.isPending || restoreUser.isPending || purgeUser.isPending;
  const actionsDisabled = batchActionPending || anyCopyDialogOpen || rowActionPending;

  if ((view === "users" || view === "deleted") && users.error && !users.data) return <AdminUserState label={users.error.message || "用户数据加载失败"} error />;
  if ((view === "users" || view === "deleted") && users.isPending) return <AdminUserState label="正在读取用户列表…" />;

  function changeView(next: UsersViewKey) {
    setPage(1);
    setSelectedUserIds([]);
    setSearchParams(next === "users" ? {} : { view: next });
  }

  function closeCreate() {
    setCreateOpen(false);
    setFormError("");
    window.requestAnimationFrame(() => createButtonRef.current?.focus());
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    if (!form.username.trim() || !form.password) {
      setFormError("用户名和密码不能为空");
      return;
    }
    create.mutate();
  }

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
    if (!window.confirm(`仅对当前页已选的 ${targets.length} 个用户应用角色预设“${preset.name}”吗？\n${names}`)) return;
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

  function confirmDisable(user: AdminUser) {
    const displayName = user.displayName || user.usernameNormalized;
    if (actionsDisabled || user.isDisabled) return;
    if (!window.confirm(`确定要禁用账户“${displayName}”吗？账户数据会保留，但用户将无法登录。`)) return;
    setActionError("");
    disableUser.mutate(user.id);
  }

  function confirmEnable(user: AdminUser) {
    const displayName = user.displayName || user.usernameNormalized;
    if (actionsDisabled || !user.isDisabled) return;
    if (!window.confirm(`确定要启用账户“${displayName}”吗？账户将恢复登录。`)) return;
    setActionError("");
    enableUser.mutate(user.id);
  }

  function confirmDelete(user: AdminUser) {
    const displayName = user.displayName || user.usernameNormalized;
    const retention = deletionGraceDays === undefined ? "按服务器当前配置的期限保留" : `保留 ${deletionGraceDays} 天`;
    if (actionsDisabled) return;
    if (!window.confirm(`确定将账户“${displayName}”移入回收站吗？账号会${retention}，你可以在期限内恢复。`)) return;
    setActionError("");
    deleteUser.mutate(user.id);
  }

  function confirmPurge(user: AdminUser) {
    const displayName = user.displayName || user.usernameNormalized;
    if (actionsDisabled) return;
    if (!window.confirm(`彻底删除“${displayName}”将永久移除账号、头像、权限设置、偏好、播放记录和设备关联，无法恢复。媒体文件不会删除。确定继续吗？`)) return;
    setActionError("");
    purgeUser.mutate(user.id);
  }

  function confirmRestore(user: AdminUser) {
    if (actionsDisabled) return;
    setActionError("");
    restoreUser.mutate(user.id);
  }

  function openPermissionCopy(user: AdminUser) {
    setSelectedUserIds([user.id]);
    setOpenMenuId(null);
    setPermissionCopyDialogOpen(true);
  }

  function openPreferenceCopy(user: AdminUser) {
    setSelectedUserIds([user.id]);
    setOpenMenuId(null);
    setCopyDialogOpen(true);
  }

  function closePermissionCopyDialog() {
    setPermissionCopyDialogOpen(false);
    window.requestAnimationFrame(() => {
      if (permissionCopyButtonRef.current && !permissionCopyButtonRef.current.disabled) permissionCopyButtonRef.current.focus();
      else if (batchPanelHeadingRef.current) batchPanelHeadingRef.current.focus();
      else listHeadingRef.current?.focus();
    });
  }

  return (
    <div className="lux-admin-page lux-users-page">
      <header className="lux-users-heading">
        <div>
          <span className="lux-admin-eyebrow">账户与访问</span>
          <h1>用户与权限</h1>
          <p>管理工作区里的账户、审批与访问策略。</p>
        </div>
        <div className="lux-users-heading-actions">
          <button className="lux-button lux-button-secondary" type="button" onClick={() => changeView("invitations")}>邀请成员</button>
          {view === "users" ? (
            <button ref={createButtonRef} className="lux-button lux-button-primary" type="button" onClick={() => { setFormError(""); setCreateOpen(true); }}>
              <Plus size={16} aria-hidden="true" />创建用户
            </button>
          ) : null}
        </div>
      </header>
      <UsersWorkspace activeView={view}>
        {view === "pending" ? <AdminUserRegistrationsPanel /> : view === "invitations" ? <AdminUserInvitationsPanel /> : (
          <>
            <section className="lux-users-toolbar" aria-label={`${VIEW_LABELS[view]}筛选`}>
              <label className="lux-users-search">
                <Search size={15} aria-hidden="true" />
                <input
                  type="search"
                  aria-label="搜索用户"
                  placeholder="搜索用户名、显示名或邮箱"
                  value={searchInput}
                  onChange={(event) => setSearchInput(event.target.value)}
                />
              </label>
              {view === "users" ? (
                <div className="lux-users-chips" role="group" aria-label="账号状态">
                  {([["all", "全部"], ["active", "正常"], ["disabled", "已禁用"]] as const).map(([value, label]) => (
                    <button key={value} type="button" aria-pressed={status === value} onClick={() => setStatus(value)}>{label}</button>
                  ))}
                </div>
              ) : (
                <span className="lux-users-tag">按删除时间排序</span>
              )}
              <div className="lux-users-toolbar-end">
                <select className="lux-users-select" aria-label="排序方式" value={sortBy} onChange={(event) => setSortBy(event.target.value as AdminUserSortBy)}>
                  <option value="lastActivityAt">最近活动</option>
                  <option value="lastLoginAt">最近登录</option>
                  <option value="createdAt">创建时间</option>
                </select>
                <button
                  className="lux-icon-button lux-icon-button-small"
                  type="button"
                  aria-label={sortOrder === "desc" ? "当前最新优先，切换为最早优先" : "当前最早优先，切换为最新优先"}
                  title={sortOrder === "desc" ? "最新优先" : "最早优先"}
                  onClick={() => setSortOrder((order) => order === "desc" ? "asc" : "desc")}
                >
                  {sortOrder === "desc" ? <ArrowDown size={15} aria-hidden="true" /> : <ArrowUp size={15} aria-hidden="true" />}
                </button>
              </div>
            </section>
            <section className="lux-users-card lux-users-table-card" aria-labelledby="lux-users-list-title">
              <div className="lux-users-card-heading">
                <div>
                  <h2 id="lux-users-list-title" ref={listHeadingRef} tabIndex={-1}>{view === "deleted" ? "已删除用户" : "全部用户"}</h2>
                  <p>{view === "deleted" ? `账号会保留 ${deletionGraceDays ?? "配置的"} 天后自动清理。` : "点击行内的「⋯」执行启用、禁用或移入回收站。"}</p>
                </div>
                <span className="lux-status-pill">共 {totalCount} 个用户</span>
              </div>
              {actionError ? <p className="lux-error-copy" role="alert" style={{ padding: "10px 20px 0" }}>{actionError}</p> : null}
              {users.error ? <p className="lux-error-copy" role="alert" style={{ padding: "10px 20px 0" }}>列表更新失败：{users.error.message}</p> : null}
              {items.length ? (
                <>
                  <div className="lux-users-table-scroll">
                    <table className="lux-users-table" aria-label={view === "deleted" ? "已删除用户列表" : "用户列表"}>
                      <thead>
                        <tr>
                          {view === "users" ? (
                            <th className="lux-users-col-check" scope="col">
                              <input
                                type="checkbox"
                                aria-label={allCurrentPageSelected ? "取消本页全选" : "全选本页"}
                                checked={allCurrentPageSelected}
                                disabled={!items.length || actionsDisabled}
                                onChange={toggleCurrentPageSelection}
                              />
                            </th>
                          ) : null}
                          <th scope="col">用户</th>
                          {view === "users" ? <th scope="col" className="lux-users-col-access">角色与访问</th> : <th scope="col">删除于</th>}
                          {view === "users" ? (
                            <th scope="col">
                              <button type="button" onClick={() => setSortOrder((order) => order === "desc" ? "asc" : "desc")} aria-label="按最近活动排序">
                                最近活动 {sortOrder === "desc" ? "↓" : "↑"}
                              </button>
                            </th>
                          ) : <th scope="col">计划清理</th>}
                          {view === "users" ? <th scope="col">状态</th> : null}
                          <th scope="col" className="lux-users-col-actions">操作</th>
                        </tr>
                      </thead>
                      <tbody>
                        {items.map((user) => view === "deleted" ? (
                          <tr key={user.id} className="lux-users-row">
                            <td>
                              <div className="lux-users-account">
                                <span className="lux-users-avatar" aria-hidden="true">{avatarInitial(user)}</span>
                                <div className="lux-users-account-copy">
                                  <strong className="lux-users-account-name">{user.displayName || user.usernameNormalized}</strong>
                                  <span className="lux-users-account-meta">{user.usernameNormalized}{user.email ? ` · ${user.email}` : ""}</span>
                                </div>
                              </div>
                            </td>
                            <td>{formatTimestamp(user.deletedAt)}</td>
                            <td>{formatTimestamp(user.deletionPurgeAt)}</td>
                            <td className="lux-users-col-actions">
                              <button className="lux-button lux-button-compact lux-button-secondary" type="button" aria-label={`恢复${user.displayName || user.usernameNormalized}账户`} onClick={() => confirmRestore(user)} disabled={actionsDisabled}>
                                <RotateCcw size={15} aria-hidden="true" />{restoreUser.isPending ? "恢复中…" : "恢复"}
                              </button>
                              <button className="lux-button lux-button-compact lux-button-secondary lux-users-danger-text" type="button" aria-label={`彻底删除${user.displayName || user.usernameNormalized}账户`} onClick={() => confirmPurge(user)} disabled={actionsDisabled}>
                                <Trash2 size={15} aria-hidden="true" />彻底删除
                              </button>
                            </td>
                          </tr>
                        ) : (
                          <ActiveUserRow
                            key={user.id}
                            user={user}
                            graceDays={deletionGraceDays}
                            selected={selectedUserIds.includes(user.id)}
                            disabled={actionsDisabled}
                            menuOpen={openMenuId === user.id}
                            onSelect={(selected) => toggleUserSelection(user.id, selected)}
                            onToggleMenu={() => setOpenMenuId((current) => current === user.id ? null : user.id)}
                            onDisable={() => confirmDisable(user)}
                            onEnable={() => confirmEnable(user)}
                            onDelete={() => confirmDelete(user)}
                            onCopyPermission={() => openPermissionCopy(user)}
                            onCopyPreference={() => openPreferenceCopy(user)}
                          />
                        ))}
                      </tbody>
                    </table>
                  </div>
                  {view === "users" ? (
                    <div className="lux-users-cards">
                      {items.map((user) => (
                        <article className="lux-users-card-item" key={user.id}>
                          <div className="lux-users-card-item-head">
                            <input
                              type="checkbox"
                              aria-label={`选择${user.displayName || user.usernameNormalized}`}
                              checked={selectedUserIds.includes(user.id)}
                              disabled={actionsDisabled}
                              onChange={(event) => toggleUserSelection(user.id, event.target.checked)}
                            />
                            <span className="lux-users-avatar" aria-hidden="true">{avatarInitial(user)}</span>
                            <div className="lux-users-account-copy">
                              <strong className="lux-users-account-name">{user.displayName || user.usernameNormalized}</strong>
                              <span className="lux-users-account-meta">@{user.usernameNormalized}</span>
                            </div>
                            <UserStatusBadge user={user} />
                            <UserMenu
                              user={user}
                              graceDays={deletionGraceDays}
                              open={openMenuId === user.id}
                              disabled={actionsDisabled}
                              onToggle={() => setOpenMenuId((current) => current === user.id ? null : user.id)}
                              onDisable={() => confirmDisable(user)}
                              onEnable={() => confirmEnable(user)}
                              onDelete={() => confirmDelete(user)}
                              onCopyPermission={() => openPermissionCopy(user)}
                              onCopyPreference={() => openPreferenceCopy(user)}
                            />
                          </div>
                          <div className="lux-users-tags">{accessTags(user)}</div>
                          <div className="lux-users-card-item-foot">
                            <span>{formatRelativeTime(user.lastActivityAt)}</span>
                            <Link className="lux-button lux-button-compact lux-button-secondary" to={`/admin/users/${encodeURIComponent(user.id)}`}>管理</Link>
                          </div>
                        </article>
                      ))}
                    </div>
                  ) : null}
                </>
              ) : (
                <p className="lux-users-empty">{view === "deleted" ? "回收站为空。" : search ? "没有找到匹配的用户。" : "当前没有用户。"}</p>
              )}
              <footer className="lux-admin-user-pagination lux-users-pager" aria-label="用户列表分页">
                <span>{totalCount ? `显示 ${(page - 1) * pageSize + 1}–${Math.min(page * pageSize, totalCount)} 条，共 ${totalCount} 条` : "共 0 条"}</span>
                <div className="lux-users-pager-actions">
                  <select aria-label="每页条数" value={pageSize} onChange={(event) => setPageSize(Number(event.target.value))}>
                    <option value={20}>每页 20 条</option>
                    <option value={50}>每页 50 条</option>
                    <option value={100}>每页 100 条</option>
                  </select>
                  <span>第 {page} / {totalPages} 页</span>
                  <button className="lux-button lux-button-compact lux-button-secondary" type="button" aria-label="上一页" disabled={page <= 1 || users.isFetching} onClick={() => setPage((current) => Math.max(1, current - 1))}>上一页</button>
                  <button className="lux-button lux-button-compact lux-button-secondary" type="button" aria-label="下一页" disabled={page >= totalPages || users.isFetching} onClick={() => setPage((current) => Math.min(totalPages, current + 1))}>下一页</button>
                </div>
              </footer>
            </section>
            {batchResults || batchStatusResults ? (
              <section className="lux-users-card" aria-label="批量结果">
                <div className="lux-users-card-heading">
                  <div><h2>批量结果</h2><p>结果保留到下一次批量操作或离开当前筛选。</p></div>
                  <button className="lux-button lux-button-compact lux-button-secondary" type="button" onClick={() => { setBatchResults(null); setBatchStatusResults(null); }}>关闭结果</button>
                </div>
                <div className="lux-users-card-body" style={{ display: "grid", gap: 12 }}>
                  {batchResults ? <div role="status" aria-label="批量应用结果"><strong style={{ fontSize: ".8rem" }}>角色预设“{batchResults.presetName}”的应用结果</strong><ul style={{ display: "grid", gap: 4, margin: "8px 0 0", padding: 0, listStyle: "none", fontSize: ".76rem" }}>{batchResults.results.map((result) => <li key={result.userId} className={result.success ? "is-success" : "is-failure"} style={{ color: result.success ? "#9bdcb3" : "var(--lux-danger, #ee9eaa)" }}>{result.userName}：{result.success ? "成功" : batchPermissionPresetErrorMessage(result.errorCode)}</li>)}</ul></div> : null}
                  {batchStatusResults ? <div role="status" aria-label="批量账号状态结果"><strong style={{ fontSize: ".8rem" }}>批量{batchStatusResults.isDisabled ? "禁用" : "启用"}结果</strong><ul style={{ display: "grid", gap: 4, margin: "8px 0 0", padding: 0, listStyle: "none", fontSize: ".76rem" }}>{batchStatusResults.results.map((result) => <li key={result.userId} className={result.success ? "is-success" : "is-failure"} style={{ color: result.success ? "#9bdcb3" : "var(--lux-danger, #ee9eaa)" }}>{result.userName}：{result.success ? "成功" : batchUserStatusErrorMessage(result.errorCode)}</li>)}</ul></div> : null}
                </div>
              </section>
            ) : null}
            {view === "users" && selectedOnCurrentPage.length > 0 ? (
              <section className="lux-users-selection" role="region" aria-label="批量操作" aria-labelledby="lux-users-batch-title">
                <strong id="lux-users-batch-title" ref={batchPanelHeadingRef} tabIndex={-1}>当前页已选 {selectedOnCurrentPage.length} 人</strong>
                <span className="lux-users-selection-hint">仅对当前页勾选的用户操作，换页或筛选后清空</span>
                <div className="lux-users-selection-divider" aria-hidden="true" />
                <button className="lux-button lux-button-compact lux-button-secondary" type="button" aria-label="批量启用用户" onClick={() => confirmBatchUserStatus(false)} disabled={batchActionPending || anyCopyDialogOpen}>{updateBatchStatus.isPending && updateBatchStatus.variables?.isDisabled === false ? "启用中…" : "批量启用"}</button>
                <button className="lux-button lux-button-compact lux-button-secondary" type="button" aria-label="批量禁用用户" onClick={() => confirmBatchUserStatus(true)} disabled={batchActionPending || anyCopyDialogOpen}>{updateBatchStatus.isPending && updateBatchStatus.variables?.isDisabled === true ? "禁用中…" : "批量禁用"}</button>
                <button className="lux-button lux-button-compact lux-button-secondary" type="button" aria-label="复制偏好" onClick={() => setCopyDialogOpen(true)} disabled={batchActionPending || anyCopyDialogOpen}>复制偏好</button>
                <button ref={permissionCopyButtonRef} className="lux-button lux-button-compact lux-button-secondary" type="button" aria-label="复制权限" onClick={() => setPermissionCopyDialogOpen(true)} disabled={batchActionPending || anyCopyDialogOpen}>复制权限</button>
                <label>角色预设
                  <select aria-label="批量角色预设" value={batchPresetId} onChange={(event) => setBatchPresetId(event.target.value)} disabled={permissionPresets.isPending || batchActionPending || anyCopyDialogOpen}>
                    <option value="">选择角色预设</option>
                    {(permissionPresets.data?.presets ?? []).map((preset) => <option key={preset.id} value={preset.id}>{preset.name}</option>)}
                  </select>
                </label>
                <button className="lux-button lux-button-primary lux-button-compact" type="button" aria-label="批量应用角色预设" onClick={confirmBatchPreset} disabled={!batchPresetId || !permissionPresets.data?.presets.some((preset) => preset.id === batchPresetId) || batchActionPending || anyCopyDialogOpen}>{applyBatchPreset.isPending ? "应用中…" : "应用预设"}</button>
                <span className="lux-users-selection-spacer" />
                <button className="lux-button lux-button-compact lux-button-secondary" type="button" onClick={() => setSelectedUserIds([])}>清除选择</button>
                {permissionPresets.error ? <p className="lux-error-copy" role="alert" style={{ flexBasis: "100%" }}>角色预设加载失败：{permissionPresets.error.message}</p> : null}
                {batchError ? <p className="lux-error-copy" role="alert" style={{ flexBasis: "100%" }}>批量应用失败：{batchError}</p> : null}
                {batchStatusError ? <p className="lux-error-copy" role="alert" style={{ flexBasis: "100%" }}>批量状态更新失败：{batchStatusError}</p> : null}
              </section>
            ) : null}
          </>
        )}
      </UsersWorkspace>
      {createOpen ? (
        <>
          <div className="lux-users-drawer-scrim" onClick={closeCreate} aria-hidden="true" />
          <aside className="lux-users-drawer" role="dialog" aria-modal="true" aria-label="创建用户">
            <div className="lux-users-drawer-head">
              <div><h2>创建用户</h2><p>创建后可在用户详情里配置权限，或直接应用角色预设。</p></div>
              <button className="lux-icon-button lux-icon-button-small" type="button" aria-label="关闭创建用户" onClick={closeCreate}><X size={16} aria-hidden="true" /></button>
            </div>
            <form className="lux-admin-form lux-users-drawer-form" onSubmit={submit} aria-label="创建用户">
              <div className="lux-users-drawer-body">
                <label>用户名<input ref={createFirstFieldRef} value={form.username} onChange={(event) => setForm({ ...form, username: event.target.value })} autoComplete="off" /></label>
                <label>显示名称<input value={form.displayName} onChange={(event) => setForm({ ...form, displayName: event.target.value })} /></label>
                <label>初始密码<input type="password" value={form.password} onChange={(event) => setForm({ ...form, password: event.target.value })} autoComplete="new-password" /></label>
                <label className="lux-admin-toggle"><input type="checkbox" checked={form.isAdmin} onChange={(event) => setForm({ ...form, isAdmin: event.target.checked })} /><span>服务器管理员</span></label>
                <p className="lux-admin-muted" style={{ margin: 0, fontSize: ".72rem", lineHeight: 1.55 }}>用户名创建后不可修改；初始密码需要用户首次登录后自行更换。</p>
                {formError ? <p className="lux-error-copy" role="alert">{formError}</p> : null}
              </div>
              <div className="lux-users-drawer-foot">
                <button className="lux-button lux-button-secondary" type="button" onClick={closeCreate}>取消</button>
                <button className="lux-button lux-button-primary" type="submit" disabled={create.isPending}>{create.isPending ? "创建中…" : "创建用户"}</button>
              </div>
            </form>
          </aside>
        </>
      ) : null}
      {copyDialogOpen ? <AdminUserPreferenceCopyDialog
        targets={selectedOnCurrentPage.map((user) => ({ id: user.id, name: user.displayName || user.usernameNormalized }))}
        onClose={() => setCopyDialogOpen(false)}
        onApplied={() => {
          setSelectedUserIds([]);
          void invalidateUsers();
        }}
      /> : null}
      {permissionCopyDialogOpen ? <AdminUserPermissionCopyDialog
        targets={selectedOnCurrentPage.map((user) => ({ id: user.id, name: user.displayName || user.usernameNormalized }))}
        onClose={closePermissionCopyDialog}
        onApplied={() => {
          setSelectedUserIds([]);
          void invalidateUsers();
        }}
      /> : null}
    </div>
  );
}

function ActiveUserRow({ user, graceDays, selected, disabled, menuOpen, onSelect, onToggleMenu, onDisable, onEnable, onDelete, onCopyPermission, onCopyPreference }: {
  user: AdminUser;
  graceDays?: number;
  selected: boolean;
  disabled: boolean;
  menuOpen: boolean;
  onSelect: (selected: boolean) => void;
  onToggleMenu: () => void;
  onDisable: () => void;
  onEnable: () => void;
  onDelete: () => void;
  onCopyPermission: () => void;
  onCopyPreference: () => void;
}) {
  const displayName = user.displayName || user.usernameNormalized;
  return (
    <tr className={`lux-users-row${selected ? " is-selected" : ""}${user.isDisabled ? " is-disabled" : ""}`}>
      <td className="lux-users-col-check">
        <label className="lux-admin-user-select">
          <input type="checkbox" aria-label={`选择${displayName}`} checked={selected} disabled={disabled} onChange={(event) => onSelect(event.target.checked)} />
          <span className="lux-visually-hidden">选择账户参与当前页批量操作</span>
        </label>
      </td>
      <td>
        <div className="lux-users-account">
          <span className="lux-users-avatar" aria-hidden="true">{avatarInitial(user)}</span>
          <div className="lux-users-account-copy">
            <strong className="lux-users-account-name">{displayName}</strong>
            <span className="lux-users-account-meta">@{user.usernameNormalized}{user.email ? ` · ${user.email}` : ""}</span>
          </div>
        </div>
      </td>
      <td className="lux-users-col-access">
        <div className="lux-users-tags">{accessTags(user)}</div>
      </td>
      <td><RelativeTime value={user.lastActivityAt} /></td>
      <td><UserStatusBadge user={user} /></td>
      <td className="lux-users-col-actions">
        <Link className="lux-button lux-button-compact lux-button-secondary" to={`/admin/users/${encodeURIComponent(user.id)}`}>管理</Link>
        <UserMenu
          user={user}
          graceDays={graceDays}
          open={menuOpen}
          disabled={disabled}
          onToggle={onToggleMenu}
          onDisable={onDisable}
          onEnable={onEnable}
          onDelete={onDelete}
          onCopyPermission={onCopyPermission}
          onCopyPreference={onCopyPreference}
        />
      </td>
    </tr>
  );
}

function UserMenu({ user, graceDays, open, disabled, onToggle, onDisable, onEnable, onDelete, onCopyPermission, onCopyPreference }: {
  user: AdminUser;
  graceDays?: number;
  open: boolean;
  disabled: boolean;
  onToggle: () => void;
  onDisable: () => void;
  onEnable: () => void;
  onDelete: () => void;
  onCopyPermission: () => void;
  onCopyPreference: () => void;
}) {
  const displayName = user.displayName || user.usernameNormalized;
  return (
    <div className="lux-users-menu-wrap" data-user-menu>
      <button className="lux-icon-button lux-icon-button-small" type="button" aria-label={`更多操作：${displayName}`} aria-expanded={open} aria-haspopup="menu" onClick={onToggle} disabled={disabled}>
        <MenuDots />
      </button>
      {open ? (
        <div className="lux-users-menu" role="menu">
          <Link role="menuitem" to={`/admin/users/${encodeURIComponent(user.id)}`}>查看详情</Link>
          <button role="menuitem" type="button" onClick={onCopyPermission}><ShieldCheck size={14} aria-hidden="true" />复制权限…</button>
          <button role="menuitem" type="button" onClick={onCopyPreference}><KeyRound size={14} aria-hidden="true" />复制偏好…</button>
          <div className="lux-users-menu-divider" role="separator" />
          {user.isDisabled ? (
            <button role="menuitem" type="button" aria-label={`启用${displayName}账户`} onClick={onEnable}><UserCheck size={14} aria-hidden="true" />启用用户</button>
          ) : (
            <button role="menuitem" type="button" aria-label={`禁用${displayName}账户`} onClick={onDisable}><Ban size={14} aria-hidden="true" />禁用用户</button>
          )}
          <button role="menuitem" className="is-danger" type="button" aria-label={`删除${displayName}账户`} title={graceDays === undefined ? "按当前服务端设置移入回收站" : `移入回收站，保留 ${graceDays} 天`} onClick={onDelete}><Trash2 size={14} aria-hidden="true" />移入回收站</button>
        </div>
      ) : null}
    </div>
  );
}

function UserStatusBadge({ user }: { user: AdminUser }) {
  return (
    <span className="lux-users-status">
      {user.isDisabled
        ? <span className="lux-user-badge is-warn">已禁用</span>
        : <span className="lux-user-badge is-ok">正常</span>}
      {(user.currentPlaybackCount ?? 0) > 0 ? <span className="lux-user-badge">播放中 {user.currentPlaybackCount}</span> : null}
    </span>
  );
}

function accessTags(user: AdminUser) {
  return (
    <>
      {user.canManageServer ? <span className="lux-users-tag is-strong">管理员</span> : null}
      <span className="lux-users-tag">{user.canRemoteAccess ? "远程访问" : "仅本地"}</span>
      {user.canDownload ? <span className="lux-users-tag">允许下载</span> : null}
    </>
  );
}

function RelativeTime({ value }: { value?: number | null }) {
  if (!value) return <span style={{ color: "var(--lux-subtle)" }}>暂无</span>;
  const date = new Date(value * 1000);
  return <time dateTime={date.toISOString()} title={date.toLocaleString()}>{formatRelativeTime(value)}</time>;
}

function avatarInitial(user: AdminUser): string {
  const name = user.displayName || user.usernameNormalized || "?";
  return name.trim().slice(0, 1).toUpperCase();
}

function MenuDots() {
  return (
    <svg width="17" height="17" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
      <circle cx="6" cy="12" r="1.6" /><circle cx="12" cy="12" r="1.6" /><circle cx="18" cy="12" r="1.6" />
    </svg>
  );
}

function AdminUserState({ label, error = false }: { label: string; error?: boolean }) {
  return <section className="lux-admin-page-state" role={error ? "alert" : "status"}><h1>{error ? "用户数据加载失败" : "正在加载用户"}</h1><p>{label}</p></section>;
}

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

