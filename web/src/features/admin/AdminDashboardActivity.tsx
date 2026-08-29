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
  const remoteIp = event.remoteIp || metadataText(event.metadata, "remoteIp");
  const location = locationDetail(event.remoteIpLocation);
  return (
    <li className="lux-admin-activity-row">
      <span className={`lux-admin-activity-icon is-${activityTone(event.eventType)}`} aria-hidden="true">{activityIcon(event.eventType)}</span>
      <div className="lux-admin-activity-copy">
        <p><strong>{event.userName || "未知账户"}</strong><span>{activityLabel(event.eventType)}</span>{event.targetTitle ? <em>{event.targetTitle}</em> : null}</p>
        <div>
          <time dateTime={new Date(event.createdAt * 1000).toISOString()}>{formatActivityTime(event.createdAt)}</time>
          {detail ? <span>· {detail}</span> : null}
          {remoteIp ? <span className="lux-admin-activity-network-detail" aria-label="IP 地址">· {remoteIp}</span> : null}
          {location ? <span className="lux-admin-activity-network-detail" aria-label="IP 归属地">· {location}</span> : null}
        </div>
      </div>
    </li>
  );
}

function activityDetail(metadata?: Record<string, unknown>) {
  if (!metadata) return undefined;
  const device = metadataText(metadata, "deviceName");
  const client = metadataText(metadata, "client");
  const version = metadataText(metadata, "clientVersion");
  const clientLabel = client && version ? `${client} v${version}` : client;
  return [device, clientLabel].filter(Boolean).join(" · ") || undefined;
}

function metadataText(metadata: Record<string, unknown> | undefined, key: string) {
  const value = metadata?.[key];
  return typeof value === "string" && value.trim() ? value : undefined;
}

function locationDetail(location: AdminActivityEvent["remoteIpLocation"]) {
  if (!location) return undefined;
  return [location.location, location.district, location.street, location.isp]
    .filter((value): value is string => Boolean(value?.trim()))
    .join(" · ") || undefined;
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
