import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { api } from "../../../../../lib/api/client";
import type { AdminUser, AdminUserWebSession } from "../../../../../lib/api/types";
import { LuxButton, useLuxConfirm } from "../../../../../components/ui";
import { AdminUserManagementState } from "../../AdminUserManagementState";
import { WebSessionDate } from "./WebSessionDate";

export function AdminUserWebSessionsPanel({ user }: { user: AdminUser }) {
  const confirm = useLuxConfirm();
  const queryClient = useQueryClient();
  const [page, setPage] = useState(1);
  const [notice, setNotice] = useState("");
  const [actionError, setActionError] = useState("");
  const sessionsQueryKey = ["admin", "user", user.id, "web-sessions", page];
  const sessionsQuery = useQuery({
    queryKey: sessionsQueryKey,
    queryFn: () => api.adminUserWebSessions(user.id, page, 20),
  });
  const revokeSession = useMutation({
    mutationFn: (sessionId: string) => api.revokeAdminUserWebSession(user.id, sessionId),
    onSuccess: async (_result) => {
      setActionError("");
      setNotice("Web 登录已撤销。");
      if (page > 1 && sessionsQuery.data?.sessions.length === 1) {
        setPage((currentPage) => Math.max(1, currentPage - 1));
      }
      await queryClient.invalidateQueries({
        queryKey: ["admin", "user", user.id, "web-sessions"],
      });
    },
    onError: (cause) => {
      setNotice("");
      setActionError(cause.message || "撤销 Web 登录失败，请重试。");
    },
  });
  const sessionPage = sessionsQuery.data;
  const pageCount = sessionPage
    ? Math.max(1, Math.ceil(sessionPage.totalCount / sessionPage.pageSize))
    : 1;

  if (sessionsQuery.isPending) {
    return <AdminUserManagementState label="正在读取 Web 登录会话…" />;
  }
  if (sessionsQuery.error) {
    return (
      <section className="lux-admin-panel lux-admin-user-sessions lux-admin-user-web-sessions" aria-labelledby="lux-admin-user-sessions-title">
        <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-sessions-title">Web 登录</h2></div></div>
        <p role="alert">读取登录会话失败：{sessionsQuery.error.message}</p>
        <LuxButton type="button" onClick={() => void sessionsQuery.refetch()}>重试</LuxButton>
      </section>
    );
  }
  if (!sessionPage) {
    return <AdminUserManagementState label="暂时无法读取 Web 登录会话。" error />;
  }

  async function requestRevoke(session: AdminUserWebSession) {
    if (session.isCurrent || revokeSession.isPending) return;
    if (!(await confirm("撤销此 Web 登录后，该浏览器下次访问时需要重新登录。确定撤销吗？"))) return;
    setNotice("");
    setActionError("");
    revokeSession.mutate(session.id);
  }

  return (
    <section className="lux-admin-panel lux-admin-user-sessions lux-admin-user-web-sessions" aria-labelledby="lux-admin-user-sessions-title">
      <div className="lux-admin-panel-heading">
        <div>
          <h2 id="lux-admin-user-sessions-title">Web 登录</h2>
          <p className="lux-admin-user-session-summary">{sessionPage.totalCount} 个有效 Web 登录</p>
        </div>
      </div>
      {actionError ? <p role="alert">{actionError}</p> : null}
      {notice ? <p role="status">{notice}</p> : null}
      {sessionPage.sessions.length === 0 ? (
        <p className="lux-admin-muted">当前没有有效的 Web 登录会话。</p>
      ) : (
        <div className="lux-admin-user-session-table-wrap">
          <table className="lux-admin-user-session-table">
            <thead>
              <tr>
                <th scope="col">登录类型</th>
                <th scope="col">创建时间</th>
                <th scope="col">最近活动</th>
                <th scope="col">到期时间</th>
                <th scope="col">状态</th>
                <th scope="col">操作</th>
              </tr>
            </thead>
            <tbody>
              {sessionPage.sessions.map((session) => (
                <tr key={session.id}>
                  <th scope="row">浏览器 Web 登录</th>
                  <td><WebSessionDate value={session.createdAt} /></td>
                  <td>{session.lastSeenAt == null ? "最近活动未知" : <WebSessionDate value={session.lastSeenAt} />}</td>
                  <td><WebSessionDate value={session.expiresAt} /></td>
                  <td>
                    {session.isCurrent
                      ? <span className="lux-user-badge is-ok">当前会话</span>
                      : <span className="lux-admin-user-session-other">其他登录</span>}
                  </td>
                  <td>
                    {session.isCurrent
                      ? <span className="lux-admin-user-session-protected">当前登录不可撤销</span>
                      : (
                        <LuxButton className="lux-admin-user-session-revoke"
                          type="button"
                          disabled={revokeSession.isPending}
                          onClick={() => requestRevoke(session)}
                        >
                          {revokeSession.isPending && revokeSession.variables === session.id ? "撤销中…" : "撤销登录"}
                        </LuxButton>
                      )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      <nav className="lux-admin-user-session-pagination" aria-label="Web 登录分页">
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
