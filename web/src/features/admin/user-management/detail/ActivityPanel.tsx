import { useQuery } from "@tanstack/react-query";
import { FormEvent, useState } from "react";
import { api } from "../../../../lib/api/client";
import type { AdminUser, AdminUserActivityEvent } from "../../../../lib/api/types";
import { LuxButton, LuxSelectField, LuxTextField } from "../../../../components/ui";
import { WebSessionDate } from "./sessions/WebSessionDate";

const activityEventLabels: Record<string, string> = {
  AUTH_LOGIN: "成功登录",
  AUTH_LOGIN_FAILED: "登录失败",
  PLAYBACK_STARTED: "开始播放",
  PLAYBACK_PAUSED: "暂停播放",
  PLAYBACK_STOPPED: "停止播放",
  USER_ACCESS_BLACKLIST_HIT: "访问黑名单命中",
  USER_ACCESS_RULES_DENIED: "访问规则拒绝",
};

const activityMetadataFields = [
  ["deviceName", "设备"],
  ["deviceType", "设备类型"],
  ["client", "客户端"],
  ["clientVersion", "客户端版本"],
  ["state", "播放状态"],
  ["remoteIp", "IP"],
  ["reason", "命中原因"],
] as const;

export function AdminUserActivityPanel({ user }: { user: AdminUser }) {
  const [page, setPage] = useState(1);
  const [eventTypeDraft, setEventTypeDraft] = useState("");
  const [fromDraft, setFromDraft] = useState("");
  const [toDraft, setToDraft] = useState("");
  const [filters, setFilters] = useState<{ eventType?: string; from?: number; to?: number }>({});
  const [filterError, setFilterError] = useState("");
  const activityQuery = useQuery({
    queryKey: ["admin", "user", user.id, "activity", page, filters],
    queryFn: () => api.adminUserActivity(user.id, { page, pageSize: 20, ...filters }),
  });
  const activityPage = activityQuery.data;
  const pageCount = activityPage
    ? Math.max(1, Math.ceil(activityPage.totalCount / activityPage.pageSize))
    : 1;

  function applyFilters(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const from = dateTimeLocalToUnix(fromDraft);
    const to = dateTimeLocalToUnix(toDraft, true);
    if ((fromDraft && from == null) || (toDraft && to == null)) {
      setFilterError("请输入有效的起止时间。");
      return;
    }
    if (from != null && to != null && from > to) {
      setFilterError("开始时间不能晚于结束时间。");
      return;
    }
    setFilterError("");
    setPage(1);
    setFilters({
      ...(eventTypeDraft ? { eventType: eventTypeDraft } : {}),
      ...(from != null ? { from } : {}),
      ...(to != null ? { to } : {}),
    });
  }

  function clearFilters() {
    setEventTypeDraft("");
    setFromDraft("");
    setToDraft("");
    setFilterError("");
    setPage(1);
    setFilters({});
  }

  return (
    <section className="lux-admin-panel lux-admin-user-activity" aria-labelledby="lux-admin-user-activity-title">
      <div className="lux-admin-panel-heading">
        <div>
          <h2 id="lux-admin-user-activity-title">活动记录</h2>
          {activityPage ? <p className="lux-admin-user-activity-summary">共 {activityPage.totalCount} 条</p> : null}
        </div>
      </div>
      <form className="lux-admin-user-activity-filters" onSubmit={applyFilters}>
        <label>
          事件类型
          <LuxSelectField aria-label="事件类型" value={eventTypeDraft} onChange={(event) => setEventTypeDraft(event.currentTarget.value)}>
            <option value="">全部事件</option>
            <option value="AUTH_LOGIN">成功登录</option>
            <option value="AUTH_LOGIN_FAILED">登录失败</option>
            <option value="PLAYBACK_STARTED">开始播放</option>
            <option value="PLAYBACK_PAUSED">暂停播放</option>
            <option value="PLAYBACK_STOPPED">停止播放</option>
          </LuxSelectField>
        </label>
        <label>
          开始时间
          <LuxTextField aria-label="开始时间" type="datetime-local" value={fromDraft} onChange={(event) => setFromDraft(event.currentTarget.value)} />
        </label>
        <label>
          结束时间
          <LuxTextField aria-label="结束时间" type="datetime-local" value={toDraft} onChange={(event) => setToDraft(event.currentTarget.value)} />
        </label>
        <div className="lux-admin-user-activity-filter-actions">
          <LuxButton tone="primary" type="submit">应用筛选</LuxButton>
          <LuxButton type="button" onClick={clearFilters}>清除</LuxButton>
        </div>
      </form>
      {filterError ? <p className="lux-error-copy" role="alert">{filterError}</p> : null}
      {activityQuery.isPending ? (
        <p className="lux-admin-muted" role="status" aria-busy="true">正在读取活动记录…</p>
      ) : activityQuery.error ? (
        <div className="lux-admin-user-activity-state">
          <p className="lux-error-copy" role="alert">读取活动记录失败：{activityQuery.error.message}</p>
          <LuxButton type="button" onClick={() => void activityQuery.refetch()}>重试</LuxButton>
        </div>
      ) : !activityPage ? (
        <p className="lux-admin-muted" role="status">暂时无法读取活动记录。</p>
      ) : activityPage.events.length === 0 ? (
        <p className="lux-admin-muted" role="status">没有符合条件的活动记录。</p>
      ) : (
        <div className="lux-admin-user-activity-table-wrap">
          <table className="lux-admin-user-activity-table">
            <thead>
              <tr>
                <th scope="col">事件</th>
                <th scope="col">发生时间</th>
                <th scope="col">目标</th>
                <th scope="col">设备与来源</th>
              </tr>
            </thead>
            <tbody>
              {activityPage.events.map((activity) => <AdminUserActivityRow key={activity.id} activity={activity} />)}
            </tbody>
          </table>
        </div>
      )}
      {activityPage ? (
        <nav className="lux-admin-user-session-pagination" aria-label="活动记录分页">
          <span>第 {activityPage.page} / {pageCount} 页</span>
          <span>共 {activityPage.totalCount} 条</span>
          <div>
            <LuxButton
              type="button"
              disabled={activityQuery.isFetching || activityPage.page <= 1}
              onClick={() => setPage((currentPage) => Math.max(1, currentPage - 1))}
            >上一页</LuxButton>
            <LuxButton
              type="button"
              disabled={activityQuery.isFetching || activityPage.page >= pageCount}
              onClick={() => setPage((currentPage) => currentPage + 1)}
            >下一页</LuxButton>
          </div>
        </nav>
      ) : null}
    </section>
  );
}

function AdminUserActivityRow({ activity }: { activity: AdminUserActivityEvent }) {
  const metadata = activityMetadataFields.flatMap(([key, label]) => {
    const value = activity.metadata[key];
    if (typeof value !== "string" && typeof value !== "number") return [];
    const safeValue = String(value).trim();
    return safeValue ? [{ label, value: safeValue.slice(0, 200) }] : [];
  });
  return (
    <tr>
      <th scope="row">
        {activityEventLabels[activity.eventType] ?? activity.eventType}
        <span className="lux-admin-user-activity-event-type">{activity.eventType}</span>
      </th>
      <td><WebSessionDate value={activity.occurredAt} /></td>
      <td>
        {activity.targetId
          ? <span>{activity.targetType ? activity.targetType + " · " : ""}{activity.targetId}</span>
          : <span className="lux-admin-muted">—</span>}
      </td>
      <td>
        {metadata.length > 0 ? (
          <ul className="lux-admin-user-activity-metadata">
            {metadata.map((item) => <li key={item.label}><span>{item.label}</span><span>{item.value}</span></li>)}
          </ul>
        ) : <span className="lux-admin-muted">—</span>}
      </td>
    </tr>
  );
}

function dateTimeLocalToUnix(value: string, inclusiveMinute = false): number | undefined {
  if (!value) return undefined;
  const milliseconds = new Date(value).getTime();
  return Number.isFinite(milliseconds) ? Math.floor(milliseconds / 1000) + (inclusiveMinute ? 59 : 0) : undefined;
}
