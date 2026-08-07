import { AudioLines, CircleAlert, MapPin, Monitor, Pause, Play, Radio, UserRound, Video } from "lucide-react";
import { Link } from "react-router-dom";
import type { CSSProperties, ReactNode } from "react";
import type { AdminPlaybackSession } from "../../lib/api/types";

export function AdminDashboardNowPlaying({ sessions }: { sessions: AdminPlaybackSession[] }) {
  if (sessions.length === 0) {
    return <div className="lux-admin-dashboard-empty" role="status"><Monitor size={22} /><div><strong>当前没有正在播放</strong><span>当有账户开始播放时，会在这里看到实时会话。</span></div></div>;
  }

  return <div className="lux-now-playing-grid">{sessions.map((session) => <NowPlayingCard key={session.id} session={session} />)}</div>;
}

function NowPlayingCard({ session }: { session: AdminPlaybackSession }) {
  const duration = session.durationTicks ?? 0;
  const percent = duration > 0 ? Math.min(100, Math.round((session.positionTicks / duration) * 100)) : 0;
  const source = session.source;
  const isEpisode = session.itemType === "EPISODE";
  const seriesTitle = session.seriesTitle?.trim();
  const cardTitle = isEpisode && seriesTitle ? seriesTitle : session.title;
  const subtitle = isEpisode
    ? [episodeLabel(session), seriesTitle ? session.title : undefined].filter(Boolean).join(" · ")
    : undefined;
  const detailItemId = isEpisode ? session.seriesId || session.itemId : session.itemId;
  const posterUrl = session.posterAvailable
    ? `/api/v1/items/${encodeURIComponent(session.itemId)}/images/poster`
    : undefined;

  return (
    <article className="lux-now-playing-card">
      <div className="lux-now-playing-body">
        <div className="lux-now-playing-poster">
          {posterUrl ? <img src={posterUrl} alt={`${cardTitle} 海报`} /> : <span aria-hidden="true"><Monitor size={32} /></span>}
          <span className={session.isPaused ? "lux-now-playing-poster-status is-paused" : "lux-now-playing-poster-status"}>
            {session.isPaused ? <Pause size={12} /> : <Play size={12} fill="currentColor" />}
            {session.isPaused ? "已暂停" : "正在播放"}
          </span>
        </div>

        <div className="lux-now-playing-content">
          <div className="lux-now-playing-heading">
            <div className="lux-now-playing-heading-copy">
              <Link className="lux-now-playing-title" to={`/items/${encodeURIComponent(detailItemId)}`}>{cardTitle}</Link>
            </div>
            <span className="lux-now-playing-year">{session.productionYear ?? "—"}</span>
            {subtitle ? <div className="lux-now-playing-subtitle">{subtitle}</div> : null}
          </div>

          <div className="lux-now-playing-account">
            <DeviceField
              icon={<UserRound size={15} />}
              label="用户"
              value={session.userName || "未知账户"}
            />
            <DeviceField
              icon={<Monitor size={15} />}
              label="设备"
              value={session.deviceName || "—"}
            />
            <DeviceField
              icon={<Radio size={15} />}
              label="客户端"
              value={session.client || "—"}
              detail={session.clientVersion ? `v${session.clientVersion}` : undefined}
            />
          </div>

          <div className="lux-now-playing-progress-block">
            <div className="lux-now-playing-progress-label">
              <span>{formatDuration(session.positionTicks)} / {formatDuration(duration)}</span>
              <strong>{percent}%</strong>
            </div>
            <div className="lux-now-playing-progress" aria-label={`已播放 ${percent}%`} role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={percent}>
              <span style={{ "--lux-progress": `${percent}%` } as CSSProperties} />
            </div>
          </div>
        </div>
      </div>

      <div className="lux-now-playing-facts">
        <Fact icon={<Radio size={17} />} label="来源" value={source?.qualityLabel || "—"} detail={sourceDetail(source)} />
        <Fact icon={<Video size={17} />} label="视频" value={source?.video?.codec || "—"} detail={source?.video?.title || "—"} />
        <Fact icon={<AudioLines size={17} />} label="音频" value={audioLabel(source)} detail={source?.audio?.title || "—"} />
      </div>

      <div className="lux-now-playing-network">
        <NetworkField icon={<CircleAlert size={15} />} label="IP 地址" value={session.remoteIp || "—"} />
        <NetworkField
          icon={<MapPin size={15} />}
          label="IP 归属地"
          value={session.remoteIpLocation?.location || "—"}
          detail={locationDetail(session.remoteIpLocation)}
        />
      </div>
    </article>
  );
}

function Fact({ icon, label, value, detail }: { icon: ReactNode; label: string; value: string; detail: string }) {
  return <div className="lux-now-playing-fact"><span className="lux-now-playing-fact-icon">{icon}</span><div className="lux-now-playing-fact-copy"><small>{label}：</small><strong>{value}</strong>{detail !== "—" ? <span> · {detail}</span> : null}</div></div>;
}

function DeviceField({ icon, label, value, detail }: { icon: ReactNode; label: string; value: string; detail?: string | null }) {
  return <span className="lux-now-playing-account-entry"><span className="lux-now-playing-account-entry-icon">{icon}</span><span><small>{label}</small><strong>{value}</strong>{detail ? <em>{detail}</em> : null}</span></span>;
}

function NetworkField({ icon, label, value, detail }: { icon: ReactNode; label: string; value: string; detail?: string }) {
  return <div className="lux-now-playing-network-field" role="group" aria-label={label}><span className="lux-now-playing-network-icon">{icon}</span><div><strong className={value === "—" ? "lux-now-playing-placeholder" : undefined}>{value}</strong>{detail ? <small>{detail}</small> : null}</div></div>;
}

function locationDetail(location: AdminPlaybackSession["remoteIpLocation"]) {
  if (!location) return undefined;
  return [location.district, location.street, location.isp].filter(Boolean).join(" · ") || undefined;
}

function episodeLabel(session: AdminPlaybackSession) {
  const season = session.parentIndexNumber == null ? "" : `S${String(session.parentIndexNumber).padStart(2, "0")}`;
  const episode = session.indexNumber == null ? "" : `E${String(session.indexNumber).padStart(2, "0")}`;
  return season && episode ? `${season}${episode}` : season || episode || "单集";
}

function audioLabel(source: AdminPlaybackSession["source"]) {
  if (!source?.audio) return "—";
  return [source.audio.codec, source.audio.language].filter(Boolean).join(" · ") || "音频轨道";
}

function sourceDetail(source: AdminPlaybackSession["source"]) {
  if (!source) return "—";
  return [source.container?.toUpperCase(), source.bitrate ? formatBitrate(source.bitrate) : undefined]
    .filter(Boolean)
    .join(" · ") || source.editionName || "—";
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
