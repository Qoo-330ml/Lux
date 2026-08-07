import { CircleDot, Pause, Play, Square } from "lucide-react";
import type { AdminActivityEvent } from "../../lib/api/types";

export function AdminDashboardActivity({ events }: { events: AdminActivityEvent[] }) {
  if (events.length === 0) {
    return <div className="lux-admin-dashboard-empty" role="status"><CircleDot size={22} /><div><strong>暂无账户活动</strong><span>播放事件会按时间出现在这里。</span></div></div>;
  }

  return <ol className="lux-admin-activity-list">{events.map((event) => <ActivityRow key={event.id} event={event} />)}</ol>;
}

function ActivityRow({ event }: { event: AdminActivityEvent }) {
  const detail = activityDetail(event.metadata);
  return (
    <li className="lux-admin-activity-row">
      <span className={`lux-admin-activity-icon is-${activityTone(event.eventType)}`} aria-hidden="true">{activityIcon(event.eventType)}</span>
      <div className="lux-admin-activity-copy">
        <p><strong>{event.userName || "未知账户"}</strong><span>{activityLabel(event.eventType)}</span>{event.targetTitle ? <em>{event.targetTitle}</em> : null}</p>
        <div><time dateTime={new Date(event.createdAt * 1000).toISOString()}>{formatActivityTime(event.createdAt)}</time>{detail ? <span>· {String(detail)}</span> : null}</div>
      </div>
    </li>
  );
}

function activityDetail(metadata?: Record<string, unknown>) {
  if (!metadata) return undefined;
  const device = typeof metadata.deviceName === "string" ? metadata.deviceName : undefined;
  const client = typeof metadata.client === "string" ? metadata.client : undefined;
  const version = typeof metadata.clientVersion === "string" ? metadata.clientVersion : undefined;
  const clientLabel = client && version ? `${client} v${version}` : client;
  return [device, clientLabel].filter(Boolean).join(" · ") || undefined;
}

function activityIcon(eventType: string) {
  switch (eventType) {
    case "PLAYBACK_STARTED": return <Play size={15} fill="currentColor" />;
    case "PLAYBACK_PAUSED": return <Pause size={15} />;
    case "PLAYBACK_STOPPED": return <Square size={14} fill="currentColor" />;
    default: return <CircleDot size={15} />;
  }
}

function activityTone(eventType: string) {
  switch (eventType) {
    case "PLAYBACK_STARTED": return "play";
    case "PLAYBACK_PAUSED": return "pause";
    case "PLAYBACK_STOPPED": return "stop";
    default: return "neutral";
  }
}

function activityLabel(eventType: string) {
  switch (eventType) {
    case "PLAYBACK_STARTED": return "开始播放";
    case "PLAYBACK_PAUSED": return "暂停播放";
    case "PLAYBACK_STOPPED": return "停止播放";
    default: return "更新了活动";
  }
}

function formatActivityTime(timestamp: number) {
  return new Date(timestamp * 1000).toLocaleString([], {
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}
