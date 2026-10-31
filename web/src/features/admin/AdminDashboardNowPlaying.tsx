import { CircleAlert, MapPin, Monitor, Pause, Play } from "lucide-react";
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
  const posterUrl = session.posterAvailable && session.posterItemId
    ? `/api/v1/items/${encodeURIComponent(session.posterItemId)}/images/poster`
    : undefined;

  const playMethod = playMethodLabel(session.playMethod);
  const playDetail = playMethodDetail(session);
  const sourceValue = sourceDetail(source);
  const ipValue = session.remoteIp || "—";

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
            <InfoLine label="用户" value={session.userName || "未知账户"} bold />
            <InfoLine label="设备" value={session.deviceName || "—"} />
            <InfoLine label="客户端" value={session.client || "—"} detail={session.clientVersion ? `v${session.clientVersion}` : undefined} />
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

      <div className="lux-now-playing-facts lux-now-playing-details">
        <Fact label="播放" value={playMethod} detail={playDetail} accent="play" method={session.playMethod} />
        <Fact label="来源" value={source?.qualityLabel || "—"} detail={sourceValue} accent="source" />
        <Fact label="视频" value={videoLabel(session)} detail={videoDetail(session)} accent="video" />
        <Fact label="音频" value={audioLabel(session)} detail={audioDetail(session)} accent="audio" />
      </div>

      <div className="lux-now-playing-network">
        <NetworkField icon={<CircleAlert size={14} />} label="IP 地址" value={ipValue} />
        <NetworkField
          icon={<MapPin size={14} />}
          label="IP 归属地"
          value={session.remoteIpLocation?.location || "—"}
          detail={locationDetail(session.remoteIpLocation)}
        />
      </div>
    </article>
  );
}

function InfoLine({ label, value, detail, bold }: { label: string; value: string; detail?: string | null; bold?: boolean }) {
  return (
    <div className="lux-now-playing-info-line lux-now-playing-account-entry">
      <span className="lux-now-playing-info-label"><small>{label}</small></span>
      {bold ? <strong>{value}</strong> : <span>{value}</span>}
      {detail ? <span className="lux-now-playing-info-detail"><em>{detail}</em></span> : null}
    </div>
  );
}

function Fact({ label, value, detail, accent, method }: { label: string; value: string; detail: string; accent?: string; method?: string | null }) {
  const isDirectPlay = accent === "play" && (method === "DirectPlay" || method === "DirectStream");
  const isTranscode = accent === "play" && method === "Transcode";
  const valueClass = [
    "lux-now-playing-kv-value",
    accent === "play" ? "is-play" : "",
    accent === "source" ? "is-source" : "",
    accent === "video" || accent === "audio" ? "is-av" : "",
    isDirectPlay ? "is-direct" : "",
    isTranscode ? "is-transcode" : "",
  ].filter(Boolean).join(" ");

  return (
    <div className="lux-now-playing-fact lux-now-playing-kv-row">
      <span className="lux-now-playing-kv-label"><small>{label}：</small></span>
      <span className={valueClass}>
        {accent === "play" ? <span className={`lux-now-playing-kv-dot${isDirectPlay ? " is-direct" : isTranscode ? " is-transcode" : ""}`} /> : null}
        <strong>{value}</strong>
        {detail !== "—" ? <span className="lux-now-playing-kv-detail"> · {detail}</span> : null}
      </span>
    </div>
  );
}

function NetworkField({ icon, label, value, detail }: { icon: ReactNode; label: string; value: string; detail?: string }) {
  return (
    <div className="lux-now-playing-network-field lux-now-playing-kv-row" role="group" aria-label={label}>
      <span className="lux-now-playing-network-icon" aria-hidden="true">{icon}</span>
      <div className="lux-now-playing-kv-value is-ip">
        <strong className={value === "—" ? "lux-now-playing-placeholder" : undefined}>{value}</strong>
        {detail ? <small className="lux-now-playing-kv-detail"> · {detail}</small> : null}
      </div>
    </div>
  );
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

function playMethodLabel(playMethod: AdminPlaybackSession["playMethod"]) {
  if (playMethod === "DirectPlay") return "直连播放";
  if (playMethod === "DirectStream") return "直流播放";
  if (playMethod === "Transcode") return "转码播放";
  return "—";
}

function playMethodDetail(session: AdminPlaybackSession) {
  if (session.playMethod !== "Transcode" && session.playMethod !== "DirectStream") return "—";
  const tier = session.serverTier == null ? undefined : serverTierLabel(session.serverTier);
  const container = session.output?.container?.toUpperCase();
  return ["服务端 HLS", tier, container].filter(Boolean).join(" · ") || "—";
}

function serverTierLabel(tier: number) {
  if (tier === 1) return "封装转换";
  if (tier === 2) return "音频转码";
  if (tier === 3) return "硬件转码";
  if (tier === 4) return "软件转码";
  return undefined;
}

function videoLabel(session: AdminPlaybackSession) {
  return formatCodec(session.output?.videoCodec || session.source?.video?.codec);
}

function audioLabel(session: AdminPlaybackSession) {
  const source = session.source;
  const codec = formatCodec(session.output?.audioCodec || source?.audio?.codec);
  if (codec === "—") return codec;
  return [codec, source?.audio?.language].filter(Boolean).join(" · ");
}

function videoDetail(session: AdminPlaybackSession) {
  const output = session.output;
  const source = session.source?.video;
  const original = source?.codec && output?.videoCodec && source.codec.toLowerCase() !== output.videoCodec.toLowerCase()
    ? `原始 ${formatCodec(source.codec)}`
    : undefined;
  return [original, source?.title, output?.videoBitrate ? formatTrackBitrate(output.videoBitrate) : undefined]
    .filter(Boolean)
    .join(" · ") || "—";
}

function audioDetail(session: AdminPlaybackSession) {
  const output = session.output;
  const source = session.source?.audio;
  const original = source?.codec && output?.audioCodec && source.codec.toLowerCase() !== output.audioCodec.toLowerCase()
    ? `原始 ${formatCodec(source.codec)}`
    : undefined;
  return [original, source?.title, output?.audioBitrate ? formatTrackBitrate(output.audioBitrate) : undefined]
    .filter(Boolean)
    .join(" · ") || "—";
}

function formatTrackBitrate(bitsPerSecond: number) {
  if (bitsPerSecond < 1_000_000) return `${Math.round(bitsPerSecond / 1_000)} kbps`;
  return formatBitrate(bitsPerSecond);
}

function formatCodec(codec: string | null | undefined) {
  if (!codec) return "—";
  const normalized = codec.trim().toLowerCase();
  if (normalized === "h264" || normalized === "avc") return "H.264";
  if (normalized === "hevc" || normalized === "h265") return "HEVC";
  if (normalized === "aac") return "AAC";
  return codec.toUpperCase();
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
