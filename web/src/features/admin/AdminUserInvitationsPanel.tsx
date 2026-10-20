import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { FormEvent, useRef, useState } from "react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type {
  AdminUserInvitation,
  AdminUserInvitationStatusFilter,
  AdminUserPermissionPreset,
} from "../../lib/api/types";

const PAGE_SIZE = 20;
const INVITATION_QUERY_PREFIX = ["admin", "user-invitations"] as const;
const MAIL_SETTINGS_QUERY_KEY = ["admin", "settings", "mail"] as const;

const statusOptions: Array<{ value: AdminUserInvitationStatusFilter; label: string }> = [
  { value: "all", label: "全部状态" },
  { value: "pending", label: "待处理" },
  { value: "accepted", label: "已接受" },
  { value: "revoked", label: "已撤销" },
  { value: "expired", label: "已过期" },
];

const statusLabels: Record<AdminUserInvitation["status"], string> = {
  PENDING: "待处理",
  ACCEPTED: "已接受",
  REVOKED: "已撤销",
  EXPIRED: "已过期",
};

export function AdminUserInvitationsPanel() {
  const queryClient = useQueryClient();
  const [page, setPage] = useState(1);
  const [status, setStatus] = useState<AdminUserInvitationStatusFilter>("all");
  const [recipientEmail, setRecipientEmail] = useState("");
  const [presetId, setPresetId] = useState("");
  const [createdInvitation, setCreatedInvitation] = useState<{
    invitationUrl: string;
    emailQueued: boolean;
  } | null>(null);
  const [createPending, setCreatePending] = useState(false);
  const [createError, setCreateError] = useState("");
  const [createStatus, setCreateStatus] = useState("");
  const [copyStatus, setCopyStatus] = useState("");
  const [resendKeys, setResendKeys] = useState<Record<string, string>>({});
  const [rowMessages, setRowMessages] = useState<Record<string, { text: string; isError?: boolean }>>({});
  const linkInput = useRef<HTMLTextAreaElement>(null);

  const invitations = useQuery({
    queryKey: queryKeys.adminUserInvitations(status, page),
    queryFn: () => api.adminUserInvitations({ page, pageSize: PAGE_SIZE, status }),
  });
  const presets = useQuery({
    queryKey: ["admin", "user-permission-presets"],
    queryFn: () => api.adminUserPermissionPresets(),
  });
  const libraries = useQuery({
    queryKey: queryKeys.adminLibraries,
    queryFn: () => api.adminLibraries(),
  });
  const mailSettings = useQuery({
    queryKey: MAIL_SETTINGS_QUERY_KEY,
    queryFn: () => api.adminMailSettings(),
  });
  const pageCount = Math.max(1, Math.ceil((invitations.data?.totalCount ?? 0) / PAGE_SIZE));
  const selectedPreset = presets.data?.presets.find((preset) => preset.id === presetId);

  const resend = useMutation({
    mutationFn: ({ invitationId, idempotencyKey }: { invitationId: string; idempotencyKey: string }) =>
      api.resendAdminUserInvitation(invitationId, idempotencyKey),
    onSuccess: (response, variables) => {
      setResendKeys((current) => {
        if (current[variables.invitationId] !== variables.idempotencyKey) return current;
        const next = { ...current };
        delete next[variables.invitationId];
        return next;
      });
      setRowMessages((current) => ({
        ...current,
        [variables.invitationId]: {
          text: response.emailQueued ? "邀请邮件已重新排队" : "邮件未能入队",
          isError: !response.emailQueued,
        },
      }));
      void queryClient.invalidateQueries({ queryKey: INVITATION_QUERY_PREFIX });
    },
    onError: (error, variables) => {
      setRowMessages((current) => ({
        ...current,
        [variables.invitationId]: { text: error.message, isError: true },
      }));
    },
  });

  const revoke = useMutation({
    mutationFn: (invitationId: string) => api.revokeAdminUserInvitation(invitationId),
    onSuccess: (response, invitationId) => {
      setRowMessages((current) => ({
        ...current,
        [invitationId]: { text: response.revoked ? "邀请已撤销" : "邀请状态已更新" },
      }));
      void queryClient.invalidateQueries({ queryKey: INVITATION_QUERY_PREFIX });
    },
    onError: (error, invitationId) => {
      setRowMessages((current) => ({
        ...current,
        [invitationId]: { text: error.message, isError: true },
      }));
    },
  });

  async function createInvitation(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const email = recipientEmail.trim();
    if (!email || !selectedPreset || createPending || mailSettings.isPending) return;

    setCreatedInvitation(null);
    setCreateError("");
    setCreateStatus("");
    setCopyStatus("");
    setCreatePending(true);
    try {
      // Keep the one-time link in component state only; it must not enter a query or mutation cache.
      const response = await api.createAdminUserInvitation({ recipientEmail: email, presetId: selectedPreset.id });
      setCreatedInvitation({
        invitationUrl: toShareableInvitationUrl(response.invitationUrl, mailSettings.data?.settings?.siteUrl),
        emailQueued: response.emailQueued,
      });
      setRecipientEmail("");
      setCreateStatus(response.emailQueued ? "邀请邮件已排队" : "邀请已创建，邮件未能入队；请复制链接后告知受邀用户。 ");
      void queryClient.invalidateQueries({ queryKey: INVITATION_QUERY_PREFIX });
    } catch (error) {
      setCreateError(error instanceof Error ? error.message : "创建邀请失败");
    } finally {
      setCreatePending(false);
    }
  }

  async function copyInvitationLink() {
    const invitationUrl = createdInvitation?.invitationUrl;
    if (!invitationUrl) return;
    try {
      if (!navigator.clipboard?.writeText) throw new Error("Clipboard API unavailable");
      await navigator.clipboard.writeText(invitationUrl);
      setCopyStatus("邀请链接已复制");
    } catch {
      linkInput.current?.focus();
      linkInput.current?.select();
      setCopyStatus("无法自动复制，请选中上方链接并手动复制。");
    }
  }

  function sendInvitationAgain(invitation: AdminUserInvitation) {
    const idempotencyKey = resendKeys[invitation.id] ?? createIdempotencyKey();
    setResendKeys((current) => ({ ...current, [invitation.id]: idempotencyKey }));
    setRowMessages((current) => {
      const next = { ...current };
      delete next[invitation.id];
      return next;
    });
    resend.mutate({ invitationId: invitation.id, idempotencyKey });
  }

  function revokeInvitation(invitation: AdminUserInvitation) {
    if (!window.confirm(`确定撤销发往 ${invitation.recipientEmail} 的邀请吗？对方将无法使用此邀请链接。`)) return;
    setRowMessages((current) => {
      const next = { ...current };
      delete next[invitation.id];
      return next;
    });
    revoke.mutate(invitation.id);
  }

  return (
    <div className="lux-admin-user-management-page lux-admin-user-invitations">
      <section className="lux-admin-panel lux-admin-user-profile-panel" aria-labelledby="lux-admin-invitation-create-title">
        <div className="lux-admin-panel-heading">
          <div>
            <span className="lux-admin-eyebrow">注册与邀请</span>
            <h2 id="lux-admin-invitation-create-title">创建邀请</h2>
            <p>邀请绑定指定邮箱，7 天内有效；用户验证邮箱并设置账户后即可激活。</p>
          </div>
        </div>
        <form className="lux-admin-form lux-admin-user-profile-form" aria-label="创建邀请" onSubmit={(event) => void createInvitation(event)}>
          <label>
            邀请邮箱
            <input
              aria-label="邀请邮箱"
              type="email"
              autoComplete="email"
              required
              value={recipientEmail}
              onChange={(event) => setRecipientEmail(event.target.value)}
              placeholder="person@example.com"
            />
          </label>
          <label>
            权限预设
            <select
              aria-label="邀请权限预设"
              required
              value={presetId}
              onChange={(event) => setPresetId(event.target.value)}
              disabled={presets.isPending || (presets.data?.presets.length ?? 0) === 0}
            >
              <option value="">选择权限预设</option>
              {(presets.data?.presets ?? []).map((preset) => (
                <option key={preset.id} value={preset.id}>{preset.name}（v{preset.version}）</option>
              ))}
            </select>
          </label>
          {presets.isPending ? <p className="lux-admin-muted">正在读取权限预设…</p> : null}
          {presets.error ? <p className="lux-error-copy" role="alert">权限预设加载失败：{presets.error.message}</p> : null}
          {!presets.isPending && !presets.error && !presets.data?.presets.length
            ? <p className="lux-admin-muted">请先创建可用于新用户的权限预设。</p>
            : null}
          {selectedPreset ? (
            <p className="lux-admin-muted" aria-label="预设媒体库权限">
              媒体库权限（来自“{selectedPreset.name}”预设）：{describeLibraryAccess(selectedPreset, libraries.data?.libraries ?? [])}
            </p>
          ) : null}
          {libraries.error && selectedPreset?.permissions.libraryAccessMode === "SELECTED"
            ? <p className="lux-admin-muted">媒体库名称暂不可用，将显示预设中的媒体库 ID。</p>
            : null}
          <p className="lux-admin-muted">邀请创建时会保存该预设当前版本的权限快照，之后修改预设不会改变此邀请。</p>
          <div className="lux-admin-user-profile-actions">
            <button className="lux-button lux-button-primary" type="submit" disabled={createPending || !selectedPreset || presets.isPending || !!presets.error || mailSettings.isPending}>
              {createPending ? "创建中…" : "创建邀请"}
            </button>
            {createStatus ? <span role="status">{createStatus}</span> : null}
            {createError ? <span role="alert">创建失败：{createError}</span> : null}
          </div>
        </form>
        {createdInvitation ? (
          <div className="lux-admin-user-content-index" aria-label="一次性邀请链接">
            <div className="lux-admin-panel-heading">
              <div><h2>一次性邀请链接</h2><p>此链接只在当前页面内存中保留；离开或刷新后无法恢复，请现在复制或使用邮件重发。</p></div>
            </div>
            <textarea
              ref={linkInput}
              aria-label="一次性邀请链接"
              readOnly
              rows={2}
              value={createdInvitation.invitationUrl}
              onFocus={(event) => event.currentTarget.select()}
            />
            <div className="lux-admin-user-profile-actions">
              <button className="lux-button lux-button-secondary" type="button" aria-label="复制邀请链接" onClick={() => void copyInvitationLink()}>复制链接</button>
              {copyStatus ? <span role="status">{copyStatus}</span> : null}
            </div>
          </div>
        ) : null}
      </section>

      <section className="lux-admin-panel lux-admin-user-activity" aria-labelledby="lux-admin-invitation-list-title">
        <div className="lux-admin-panel-heading">
          <div>
            <span className="lux-admin-eyebrow">账户生命周期</span>
            <h2 id="lux-admin-invitation-list-title">邀请列表</h2>
            <p>共 {invitations.data?.totalCount ?? 0} 项邀请</p>
          </div>
          <label className="lux-admin-user-activity-filter">
            邀请状态
            <select aria-label="邀请状态" value={status} onChange={(event) => { setStatus(event.target.value as AdminUserInvitationStatusFilter); setPage(1); }}>
              {statusOptions.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
            </select>
          </label>
        </div>
        {invitations.isPending ? <p className="lux-admin-muted" role="status">正在读取邀请…</p> : null}
        {invitations.error ? <p className="lux-error-copy" role="alert">邀请列表加载失败：{invitations.error.message}</p> : null}
        {!invitations.isPending && !invitations.error && invitations.data?.invitations.length === 0
          ? <p className="lux-admin-muted">此状态下没有邀请。</p>
          : null}
        {invitations.data?.invitations.length ? (
          <div className="lux-admin-user-activity-table-wrap">
            <table className="lux-admin-user-activity-table" aria-label="邀请列表">
              <thead><tr><th scope="col">邮箱</th><th scope="col">权限预设</th><th scope="col">状态</th><th scope="col">有效期至</th><th scope="col">操作</th></tr></thead>
              <tbody>
                {invitations.data.invitations.map((item) => (
                  <tr key={item.id}>
                    <th scope="row">{item.recipientEmail}</th>
                    <td>{item.presetName}（v{item.presetVersion}）</td>
                    <td>{statusLabels[item.status]}</td>
                    <td><time dateTime={new Date(item.expiresAt * 1000).toISOString()}>{formatInvitationDate(item.expiresAt)}</time></td>
                    <td>
                      {item.status === "PENDING" ? (
                        <div className="lux-admin-user-profile-actions">
                          <button
                            className="lux-button lux-button-secondary lux-button-compact"
                            type="button"
                            aria-label={`重新发送邀请给 ${item.recipientEmail}`}
                            disabled={resend.isPending || revoke.isPending}
                            onClick={() => sendInvitationAgain(item)}
                          >
                            {resend.isPending && resend.variables?.invitationId === item.id
                              ? "发送中…"
                              : resendKeys[item.id] ? "重试发送" : "重新发送"}
                          </button>
                          <button
                            className="lux-button lux-button-secondary lux-button-compact"
                            type="button"
                            aria-label={`撤销发往 ${item.recipientEmail} 的邀请`}
                            disabled={resend.isPending || revoke.isPending}
                            onClick={() => revokeInvitation(item)}
                          >撤销</button>
                        </div>
                      ) : <span className="lux-admin-muted">—</span>}
                      {rowMessages[item.id] ? (
                        <p className={rowMessages[item.id]?.isError ? "lux-error-copy" : "lux-admin-muted"} role={rowMessages[item.id]?.isError ? "alert" : "status"}>
                          {rowMessages[item.id]?.text}
                        </p>
                      ) : null}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        ) : null}
        <nav className="lux-admin-user-session-pagination" aria-label="邀请分页">
          <span>第 {page} / {pageCount} 页</span>
          <div>
            <button className="lux-button lux-button-secondary lux-button-compact" type="button" aria-label="邀请上一页" disabled={page <= 1 || invitations.isFetching} onClick={() => setPage((current) => Math.max(1, current - 1))}>上一页</button>
            <button className="lux-button lux-button-secondary lux-button-compact" type="button" aria-label="邀请下一页" disabled={page >= pageCount || invitations.isFetching} onClick={() => setPage((current) => Math.min(pageCount, current + 1))}>下一页</button>
          </div>
        </nav>
      </section>
    </div>
  );
}

function describeLibraryAccess(
  preset: AdminUserPermissionPreset,
  libraries: Array<{ id: string; name: string }>,
) {
  const { libraryAccessMode, libraryIds } = preset.permissions;
  if (libraryAccessMode === "ALL") return "全部可用媒体库";
  if (libraryAccessMode === "NONE") return "没有媒体库访问权限";
  const names = libraryIds.map((libraryId) => libraries.find((library) => library.id === libraryId)?.name ?? libraryId);
  return names.length ? names.join("、") : "未选择媒体库";
}

function formatInvitationDate(timestamp: number) {
  return new Date(timestamp * 1000).toLocaleString("zh-CN");
}

function toShareableInvitationUrl(invitationUrl: string, siteUrl?: string | null) {
  const relativePath = invitationUrl.replace(/^\/+/, "");
  const configuredBase = siteUrl || window.location.origin;
  try {
    const base = new URL(configuredBase);
    if (base.protocol !== "http:" && base.protocol !== "https:") throw new Error("Unsupported site URL scheme");
    base.pathname = `${base.pathname.replace(/\/+$/, "")}/`;
    return new URL(relativePath, base).toString();
  } catch {
    const fallback = new URL(`${window.location.origin}/`);
    return new URL(relativePath, fallback).toString();
  }
}

function createIdempotencyKey() {
  if (typeof globalThis.crypto?.randomUUID === "function") return globalThis.crypto.randomUUID();
  return `${Date.now()}-${Math.random().toString(36).slice(2)}-${Math.random().toString(36).slice(2)}`;
}
