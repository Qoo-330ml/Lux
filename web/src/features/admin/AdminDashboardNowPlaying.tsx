import { AudioLines, Gauge, MonitorPlay, Pause, Play, Video } from "lucide-react";
import { Link } from "react-router-dom";
import type { CSSProperties, ReactNode } from "react";
import type { AdminPlaybackSession } from "../../lib/api/types";

export function AdminDashboardNowPlaying({ sessions }: { sessions: AdminPlaybackSession[] }) {
  if (sessions.length === 0) {
    return <div className="lux-admin-dashboard-empty" role="status"><MonitorPlay size={22} /><div><strong>当前没有正在播放</strong><span>当有账户开始播放时，会在这里看到实时会话。</span></div></div>;
  }

  return <div className="lux-now-playing-grid">{sessions.map((session) => <NowPlayingCard key={session.id} session={session} />)}</div>;
}

function NowPlayingCard({ session }: { session: AdminPlaybackSession }) {
  const duration = session.durationTicks ?? 0;
  const percent = duration > 0 ? Math.min(100, Math.round((session.positionTicks / duration) * 100)) : 0;
  const source = session.source;
  const posterUrl = session.posterAvailable
    ? `/api/v1/items/${encodeURIComponent(session.itemId)}/images/poster`
    : undefined;

  return (
    <article className="lux-now-playing-card">
      <div className="lux-now-playing-main">
        <div className="lux-now-playing-poster">
          {posterUrl ? <img src={posterUrl} alt={`${session.title} 海报`} /> : <span aria-hidden="true"><MonitorPlay size={28} /></span>}
          <span className={session.isPaused ? "lux-now-playing-badge is-paused" : "lux-now-playing-badge"}>
            {session.isPaused ? <Pause size={12} /> : <Play size={12} fill="currentColor" />}
            {session.isPaused ? "已暂停" : "正在播放"}
          </span>
        </div>
        <div className="lux-now-playing-copy">
          <div className="lux-now-playing-user"><span className="lux-live-dot" />{session.userName}<span>·</span><span>{session.client || "未知客户端"}</span></div>
          <Link className="lux-now-playing-title" to={`/items/${encodeURIComponent(session.itemId)}`}>{session.title}</Link>
          <div className="lux-now-playing-subtitle">
            {episodeLabel(session)}{session.productionYear ? ` · ${session.productionYear}` : ""}
            {session.originalTitle && session.originalTitle !== session.title ? ` · ${session.originalTitle}` : ""}
          </div>
          <div className="lux-now-playing-progress-label"><span>{formatDuration(session.positionTicks)} / {formatDuration(duration)}</span><strong>{percent}%</strong></div>
          <div className="lux-now-playing-progress" aria-label={`已播放 ${percent}%`} role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={percent}>
            <span style={{ "--lux-progress": `${percent}%` } as CSSProperties} />
          </div>
        </div>
      </div>
      <div className="lux-now-playing-details">
        <InfoCell icon={<MonitorPlay size={15} />} label="设备" value={session.deviceName || session.deviceId} detail={session.client || "未知客户端"} />
        <InfoCell icon={<Gauge size={15} />} label="来源" value={source?.qualityLabel || "Direct Play"} detail={sourceDetail(source)} />
        <InfoCell icon={<Video size={15} />} label="视频" value={source?.video?.codec || "未探测"} detail={source?.video?.title || "视频轨道"} />
        <InfoCell icon={<AudioLines size={15} />} label="音频" value={audioLabel(source)} detail={source?.audio?.title || "音频轨道"} />
      </div>
    </article>
  );
}

function InfoCell({ icon, label, value, detail }: { icon: ReactNode; label: string; value: string; detail: string }) {
  return <div className="lux-now-playing-info"><span className="lux-now-playing-info-icon">{icon}</span><div><small>{label}</small><strong>{value}</strong><span>{detail}</span></div></div>;
}

function episodeLabel(session: AdminPlaybackSession) {
  if (session.itemType !== "EPISODE") return session.itemType === "MOVIE" ? "电影" : "媒体";
  const season = session.parentIndexNumber == null ? "" : `S${session.parentIndexNumber}`;
  const episode = session.indexNumber == null ? "" : `E${session.indexNumber}`;
  return season && episode ? `${season} · ${episode}` : season || episode || "单集";
}

function audioLabel(source: AdminPlaybackSession["source"]) {
  if (!source?.audio) return "未探测";
  return [source.audio.codec, source.audio.language].filter(Boolean).join(" · ") || "音频轨道";
}

function sourceDetail(source: AdminPlaybackSession["source"]) {
  if (!source) return "未选择媒体来源";
  return [source.container?.toUpperCase(), source.bitrate ? formatBitrate(source.bitrate) : undefined]
    .filter(Boolean)
    .join(" · ") || source.editionName || "直接播放";
}

function formatBitrate(bitsPerSecond: number) {
  const megabits = bitsPerSecond / 1_000_000;
  return `${megabits >= 10 ? Math.round(megabits) : megabits.toFixed(1)} Mbps`;
}

function formatDuration(ticks: number) {
  if (!ticks) return "00:00";
  const seconds = Math.max(0, Math.round(ticks / 10_000_000));
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60).toString().padStart(2, "0");
  const remaining = (seconds % 60).toString().padStart(2, "0");
  return hours ? `${hours}:${minutes}:${remaining}` : `${minutes}:${remaining}`;
}
