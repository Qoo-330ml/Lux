import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { api } from "../../../../../lib/api/client";
import type { AdminUser, AdminUserPlaybackSession } from "../../../../../lib/api/types";
import { LuxButton, LuxProgress, useLuxConfirm } from "../../../../../components/ui";
import { AdminUserManagementState } from "../../AdminUserManagementState";

export function AdminUserPlaybackSessionsPanel({ user }: { user: AdminUser }) {
  const confirm = useLuxConfirm();
  const queryClient = useQueryClient();
  const [page, setPage] = useState(1);
  const [notice, setNotice] = useState("");
  const [actionError, setActionError] = useState("");
  const sessionsQuery = useQuery({
    queryKey: ["admin", "user", user.id, "playback-sessions", page],
    queryFn: () => api.adminUserPlaybackSessions(user.id, page, 20),
  });
  const stopSession = useMutation({
    mutationFn: (playbackId: string) => api.stopAdminUserPlaybackSession(user.id, playbackId),
    onSuccess: async () => {
      setActionError("");
      setNotice("播放已停止。");
      if (page > 1 && sessionsQuery.data?.sessions.length === 1) {
        setPage((currentPage) => Math.max(1, currentPage - 1));
      }
      await queryClient.invalidateQueries({
        queryKey: ["admin", "user", user.id, "playback-sessions"],
      });
    },
    onError: (cause) => {
      setNotice("");
      setActionError(cause.message || "停止播放失败，请重试。");
    },
  });
  const sessionPage = sessionsQuery.data;
  const pageCount = sessionPage
    ? Math.max(1, Math.ceil(sessionPage.totalCount / sessionPage.pageSize))
    : 1;

  if (sessionsQuery.isPending) {
    return <AdminUserManagementState label="正在读取当前播放…" />;
  }
  if (sessionsQuery.error) {
    return (
      <section className="lux-admin-panel lux-admin-user-playback-sessions" aria-labelledby="lux-admin-user-playback-sessions-title">
        <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-playback-sessions-title">当前播放</h2></div></div>
        <p role="alert">读取当前播放失败：{sessionsQuery.error.message}</p>
        <LuxButton type="button" onClick={() => void sessionsQuery.refetch()}>重试</LuxButton>
      </section>
    );
  }
  if (!sessionPage) {
    return <AdminUserManagementState label="暂时无法读取当前播放。" error />;
  }

  async function requestStop(session: AdminUserPlaybackSession) {
    if (!session.canStop || stopSession.isPending) return;
    if (!(await confirm("将停止此用户通过 Lux 服务端 HLS 进行的播放。用户登录仍然有效。确定停止吗？"))) return;
    setNotice("");
    setActionError("");
    stopSession.mutate(session.id);
  }

  return (
    <section className="lux-admin-panel lux-admin-user-playback-sessions" aria-labelledby="lux-admin-user-playback-sessions-title">
      <div className="lux-admin-panel-heading">
        <div>
          <h2 id="lux-admin-user-playback-sessions-title">当前播放</h2>
          <p className="lux-admin-user-session-summary">{sessionPage.totalCount} 个活跃播放会话</p>
        </div>
      </div>
      <p className="lux-admin-muted lux-admin-user-playback-boundary">
        仅 Lux 服务端 HLS 会话可由此处停止。直放与 STRM 外部播放无法保证立即中断；停止播放不会撤销用户登录。
      </p>
      {actionError ? <p className="lux-error-copy" role="alert">停止播放失败：{actionError}</p> : null}
      {notice ? <p role="status">{notice}</p> : null}
      {sessionPage.sessions.length === 0 ? (
        <p className="lux-admin-muted" role="status">当前没有活跃的播放会话。</p>
      ) : (
        <div className="lux-admin-user-session-table-wrap">
          <table className="lux-admin-user-session-table lux-admin-user-playback-table">
            <thead>
              <tr>
                <th scope="col">媒体</th>
                <th scope="col">进度</th>
                <th scope="col">播放方式</th>
                <th scope="col">客户端与设备</th>
                <th scope="col">来源 IP</th>
                <th scope="col">状态</th>
                <th scope="col">操作</th>
              </tr>
            </thead>
            <tbody>
              {sessionPage.sessions.map((session) => (
                <tr key={session.id}>
                  <th scope="row">
                    {session.title}
                    {session.seriesTitle ? (
                      <span className="lux-admin-user-session-device-meta">
                        {session.seriesTitle}
                        {session.parentIndexNumber != null ? ` · 第 ${session.parentIndexNumber} 季` : ""}
                        {session.indexNumber != null ? ` 第 ${session.indexNumber} 集` : ""}
                      </span>
                    ) : null}
                  </th>
                  <td>
                    <div className="lux-admin-user-playback-progress">
                      {session.durationTicks != null && session.durationTicks > 0 ? (
                        <LuxProgress
                          label={`${session.title} 播放进度`}
                          max={session.durationTicks}
                          value={Math.min(Math.max(session.positionTicks, 0), session.durationTicks)}
                          size="compact"
                        />
                      ) : null}
                      <span>{playbackProgressLabel(session)}</span>
                    </div>
                  </td>
                  <td>{playbackMethodLabel(session)}</td>
                  <td>
                    {[session.client, session.clientVersion].filter(Boolean).join(" ") || "未知客户端"}
                    <span className="lux-admin-user-session-device-meta">
                      {session.deviceName || "未知设备"}{session.deviceType ? ` · ${session.deviceType}` : ""}
                    </span>
                  </td>
                  <td>
                    {session.remoteIp || "未知"}
                    {session.remoteIpLocation?.location ? (
                      <span className="lux-admin-user-session-device-meta">{session.remoteIpLocation.location}</span>
                    ) : null}
                  </td>
                  <td>{session.isPaused || session.state === "PAUSED" ? "已暂停" : "正在播放"}</td>
                  <td>
                    {session.canStop ? (
                      <LuxButton className="lux-admin-user-session-revoke lux-admin-user-playback-stop"
                        type="button"
                        disabled={stopSession.isPending}
                        onClick={() => requestStop(session)}
                      >
                        {stopSession.isPending && stopSession.variables === session.id ? "停止中…" : "停止播放"}
                      </LuxButton>
                    ) : <span className="lux-admin-user-session-protected">无法远程停止</span>}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {sessionPage.totalCount > 0 ? (
        <nav className="lux-admin-user-session-pagination" aria-label="当前播放分页">
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
      ) : null}
    </section>
  );
}

function playbackProgressLabel(session: AdminUserPlaybackSession) {
  if (session.durationTicks == null || session.durationTicks <= 0) return "进度未知";
  const position = formatPlaybackTicks(session.positionTicks);
  const duration = formatPlaybackTicks(session.durationTicks);
  const percent = Math.round(Math.min(Math.max(session.positionTicks, 0), session.durationTicks) / session.durationTicks * 100);
  return `${percent}% · ${position} / ${duration}`;
}

function formatPlaybackTicks(ticks: number) {
  const totalSeconds = Math.max(0, Math.floor(ticks / 10_000_000));
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  return hours > 0
    ? `${hours}:${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`
    : `${minutes}:${String(seconds).padStart(2, "0")}`;
}

function playbackMethodLabel(session: AdminUserPlaybackSession) {
  if (session.playbackPlan === "SERVER_HLS") return "Lux 服务端 HLS";
  if (session.playMethod === "DirectStream") return "直通播放";
  if (session.playMethod === "Transcode") return "服务端转码";
  return "直放 / 外部播放";
}
