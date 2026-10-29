import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { api } from "../../../../../lib/api/client";
import type { AdminUser, AdminUserEmbySession } from "../../../../../lib/api/types";
import { LuxButton, useLuxConfirm } from "../../../../../components/ui";
import { AdminUserManagementState } from "../../AdminUserManagementState";
import { WebSessionDate } from "./WebSessionDate";

export function AdminUserEmbySessionsPanel({ user }: { user: AdminUser }) {
  const confirm = useLuxConfirm();
  const queryClient = useQueryClient();
  const [page, setPage] = useState(1);
  const [notice, setNotice] = useState("");
  const [actionError, setActionError] = useState("");
  const sessionsQueryKey = ["admin", "user", user.id, "emby-sessions", page];
  const sessionsQuery = useQuery({
    queryKey: sessionsQueryKey,
    queryFn: () => api.adminUserEmbySessions(user.id, page, 20),
  });
  const revokeSession = useMutation({
    mutationFn: (sessionId: string) => api.revokeAdminUserEmbySession(user.id, sessionId),
    onSuccess: async () => {
      setActionError("");
      setNotice("Emby 登录已撤销。");
      if (page > 1 && sessionsQuery.data?.sessions.length === 1) {
        setPage((currentPage) => Math.max(1, currentPage - 1));
      }
      await queryClient.invalidateQueries({
        queryKey: ["admin", "user", user.id, "emby-sessions"],
      });
    },
    onError: (cause) => {
      setNotice("");
      setActionError(cause.message || "撤销 Emby 登录失败，请重试。");
    },
  });
  const revokeAllSessions = useMutation({
    mutationFn: () => api.revokeAllAdminUserEmbySessions(user.id),
    onSuccess: async ({ revokedCount }) => {
      setActionError("");
      setNotice("已撤销 " + revokedCount + " 个 Emby 登录凭证。");
      setPage(1);
      await queryClient.invalidateQueries({
        queryKey: ["admin", "user", user.id, "emby-sessions"],
      });
    },
    onError: (cause) => {
      setNotice("");
      setActionError(cause.message || "撤销全部 Emby 登录失败，请重试。");
    },
  });
  const sessionPage = sessionsQuery.data;
  const pageCount = sessionPage
    ? Math.max(1, Math.ceil(sessionPage.totalCount / sessionPage.pageSize))
    : 1;
  const totalSessionCount = sessionPage?.totalCount ?? 0;
  const isRevoking = revokeSession.isPending || revokeAllSessions.isPending;

  if (sessionsQuery.isPending) {
    return <AdminUserManagementState label="正在读取 Emby 登录凭证…" />;
  }
  if (sessionsQuery.error) {
    return (
      <section className="lux-admin-panel lux-admin-user-sessions" aria-labelledby="lux-admin-user-emby-sessions-title">
        <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-emby-sessions-title">Emby 登录</h2></div></div>
        <p role="alert">读取 Emby 登录失败：{sessionsQuery.error.message}</p>
        <LuxButton type="button" onClick={() => void sessionsQuery.refetch()}>重试</LuxButton>
      </section>
    );
  }
  if (!sessionPage) {
    return <AdminUserManagementState label="暂时无法读取 Emby 登录凭证。" error />;
  }

  async function requestRevoke(session: AdminUserEmbySession) {
    if (isRevoking) return;
    if (!(await confirm("撤销此 Emby 登录凭证后，对应客户端需要重新登录。确定撤销吗？"))) return;
    setNotice("");
    setActionError("");
    revokeSession.mutate(session.id);
  }

  async function requestRevokeAll() {
    if (isRevoking || totalSessionCount === 0) return;
    if (!(await confirm(
      "即将撤销此用户全部 " + totalSessionCount + " 个有效 Emby 登录凭证，对应客户端需要重新登录。确定继续吗？",
    ))) return;
    setNotice("");
    setActionError("");
    revokeAllSessions.mutate();
  }

  return (
    <section className="lux-admin-panel lux-admin-user-sessions" aria-labelledby="lux-admin-user-emby-sessions-title">
      <div className="lux-admin-panel-heading">
        <div>
          <h2 id="lux-admin-user-emby-sessions-title">Emby 登录</h2>
          <p className="lux-admin-user-session-summary">{sessionPage.totalCount} 个有效 Emby 登录凭证</p>
        </div>
        <LuxButton className="lux-admin-user-session-revoke-all"
          type="button"
          disabled={isRevoking || sessionsQuery.isFetching || sessionPage.totalCount === 0}
          onClick={requestRevokeAll}
        >
          {revokeAllSessions.isPending ? "正在撤销…" : "撤销全部 Emby 登录"}
        </LuxButton>
      </div>
      {actionError ? <p role="alert">{actionError}</p> : null}
      {notice ? <p role="status">{notice}</p> : null}
      {sessionPage.sessions.length === 0 ? (
        <p className="lux-admin-muted">当前没有有效的 Emby 登录凭证。</p>
      ) : (
        <div className="lux-admin-user-session-table-wrap">
          <table className="lux-admin-user-session-table lux-admin-user-emby-session-table">
            <thead>
              <tr>
                <th scope="col">设备</th>
                <th scope="col">客户端</th>
                <th scope="col">首次登录</th>
                <th scope="col">最近活动</th>
                <th scope="col">操作</th>
              </tr>
            </thead>
            <tbody>
              {sessionPage.sessions.map((session) => (
                <tr key={session.id}>
                  <th scope="row">
                    {session.deviceName || "未知设备"}
                    <span className="lux-admin-user-session-device-meta">
                      设备 ID：{session.deviceId || "未知"}
                      {session.deviceType ? " · " + session.deviceType : ""}
                    </span>
                  </th>
                  <td>{[session.clientName, session.clientVersion].filter(Boolean).join(" ") || "未知客户端"}</td>
                  <td><WebSessionDate value={session.createdAt} /></td>
                  <td>{session.lastSeenAt == null ? "最近活动未知" : <WebSessionDate value={session.lastSeenAt} />}</td>
                  <td>
                    <LuxButton className="lux-admin-user-session-revoke"
                      type="button"
                      disabled={isRevoking}
                      onClick={() => requestRevoke(session)}
                    >
                      {revokeSession.isPending && revokeSession.variables === session.id ? "撤销中…" : "撤销登录"}
                    </LuxButton>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      <nav className="lux-admin-user-session-pagination" aria-label="Emby 登录分页">
        <span>第 {sessionPage.page} / {pageCount} 页</span>
        <div>
          <LuxButton
            type="button"
            disabled={sessionsQuery.isFetching || sessionPage.page <= 1}
            onClick={() => setPage((currentPage) => Math.max(1, currentPage - 1))}
          >上一页</LuxButton>
          <LuxButton
            type="button"
            disabled={sessionsQuery.isFetching || sessionPage.page >= pageCount}
            onClick={() => setPage((currentPage) => currentPage + 1)}
          >下一页</LuxButton>
        </div>
      </nav>
    </section>
  );
}
