import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { api } from "../../../../lib/api/client";
import { queryKeys } from "../../../../lib/api/query-keys";
import type { AdminUser } from "../../../../lib/api/types";
import { LuxButton, LuxSwitch, useLuxConfirm } from "../../../../components/ui";
import { DetailDisclosure } from "../../users/DetailDisclosure";
import { formatTimestamp } from "../../users/format";

export function AdminUserLifecycleActions({ user }: { user: AdminUser }) {
  const confirm = useLuxConfirm();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [error, setError] = useState("");
  const settings = useQuery({ queryKey: queryKeys.adminSettings, queryFn: () => api.adminSettings() });
  const graceDays = settings.data?.userDeletionGraceDays;
  const refresh = (updated: AdminUser) => {
    queryClient.setQueryData(["admin", "user", user.id], { user: updated });
    void queryClient.invalidateQueries({ queryKey: queryKeys.adminUsers });
  };
  const disable = useMutation({ mutationFn: () => api.disableAdminUser(user.id), onSuccess: ({ user: updated }) => refresh(updated), onError: (cause) => setError(cause.message) });
  const enable = useMutation({ mutationFn: () => api.updateAdminUser(user.id, { isDisabled: false }), onSuccess: ({ user: updated }) => refresh(updated), onError: (cause) => setError(cause.message) });
  const remove = useMutation({ mutationFn: () => api.deleteAdminUser(user.id), onSuccess: ({ user: updated }) => { refresh(updated); navigate("/admin/users?view=deleted"); }, onError: (cause) => setError(cause.message) });
  const restore = useMutation({ mutationFn: () => api.restoreAdminUser(user.id), onSuccess: ({ user: updated }) => { refresh(updated); navigate("/admin/users"); }, onError: (cause) => setError(cause.message) });
  const purge = useMutation({ mutationFn: () => api.purgeAdminUser(user.id), onSuccess: () => { void queryClient.invalidateQueries({ queryKey: queryKeys.adminUsers }); navigate("/admin/users?view=deleted"); }, onError: (cause) => setError(cause.message) });
  const updateRole = useMutation({ mutationFn: (canManageServer: boolean) => api.updateAdminUser(user.id, { canManageServer }), onSuccess: ({ user: updated }) => refresh(updated), onError: (cause) => setError(cause.message) });
  const pending = disable.isPending || enable.isPending || remove.isPending || restore.isPending || purge.isPending || updateRole.isPending;
  const displayName = user.displayName || user.usernameNormalized;

  async function confirmDisable() {
    if (pending || !(await confirm(`禁用“${displayName}”后会撤销登录凭证并停止播放，账号资料仍会保留。确定继续吗？`))) return;
    setError("");
    disable.mutate();
  }

  async function confirmEnable() {
    if (pending || !(await confirm(`启用“${displayName}”后，用户可以重新登录。确定继续吗？`))) return;
    setError("");
    enable.mutate();
  }

  async function confirmDelete() {
    const retention = graceDays === undefined ? "按服务器当前配置的期限保留" : `保留 ${graceDays} 天`;
    if (pending || !(await confirm(`将“${displayName}”移入回收站会撤销凭证并停止播放，账号会${retention}，可在期限内恢复。确定继续吗？`))) return;
    setError("");
    remove.mutate();
  }

  async function confirmPurge() {
    if (pending || !(await confirm(`彻底删除“${displayName}”将永久删除账号、头像、权限设置、偏好、播放记录和设备关联，媒体文件不受影响，且无法恢复。确定继续吗？`))) return;
    setError("");
    purge.mutate();
  }

  async function confirmManageServer(next: boolean) {
    if (pending || user.deletedAt != null) return;
    const prompt = next
      ? `将“${displayName}”设为服务器管理员？该用户可以管理媒体库、插件、服务器设置与其他用户。`
      : `取消“${displayName}”的服务器管理员权限？该用户将不能再管理服务器。`;
    if (!(await confirm(prompt))) return;
    setError("");
    updateRole.mutate(next);
  }

  const statusLabel = user.deletedAt != null ? "回收站" : user.isDisabled ? "已禁用" : "正常";

  return (
    <section className="lux-admin-panel lux-admin-user-lifecycle-panel" aria-labelledby="lux-admin-user-lifecycle-title">
      <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-lifecycle-title">账号状态</h2></div><span className={user.deletedAt != null || user.isDisabled ? "lux-user-badge is-warn" : "lux-user-badge is-ok"}>{statusLabel}</span></div>
      <dl className="lux-admin-user-lifecycle-facts">
        <div><dt>登录状态</dt><dd>{statusLabel}</dd></div>
        <div><dt>角色</dt><dd>{user.canManageServer ? "服务器管理员" : "普通用户"}</dd></div>
        {user.deletedAt != null ? <div><dt>移入回收站</dt><dd>{formatTimestamp(user.deletedAt)}</dd></div> : null}
        {user.deletedAt != null && user.deletionPurgeAt != null ? <div><dt>计划清理</dt><dd>{formatTimestamp(user.deletionPurgeAt)}</dd></div> : null}
      </dl>
      <DetailDisclosure
        id={`user-lifecycle-${user.id}`}
        openLabel="账号操作"
        closeLabel="收起操作"
        hint="禁用、移入回收站、恢复与彻底删除只在需要时展开。"
      >
        {user.deletedAt == null ? (
          <LuxSwitch className="lux-admin-toggle" style={{ marginBottom: 14 }} checked={user.canManageServer} disabled={pending} onChange={(event) => confirmManageServer(event.target.checked)} label="服务器管理员" />
        ) : null}
        <div className="lux-admin-user-profile-actions">
          {user.deletedAt != null ? <>
            <LuxButton tone="secondary" type="button" onClick={() => restore.mutate()} disabled={pending}>{restore.isPending ? "恢复中…" : "恢复账号"}</LuxButton>
            <LuxButton tone="secondary" className="lux-admin-purge-user" type="button" onClick={confirmPurge} disabled={pending}>{purge.isPending ? "删除中…" : "彻底删除"}</LuxButton>
          </> : <>
            {user.isDisabled
              ? <LuxButton tone="secondary" type="button" onClick={confirmEnable} disabled={pending}>{enable.isPending ? "启用中…" : "启用账号"}</LuxButton>
              : <LuxButton tone="secondary" type="button" onClick={confirmDisable} disabled={pending}>{disable.isPending ? "禁用中…" : "禁用账号"}</LuxButton>}
            <LuxButton tone="secondary" className="lux-admin-purge-user" type="button" onClick={confirmDelete} disabled={pending}>{remove.isPending ? "移入回收站…" : "移入回收站"}</LuxButton>
          </>}
          {error ? <span role="alert">操作失败：{error}</span> : null}
        </div>
      </DetailDisclosure>
    </section>
  );
}
