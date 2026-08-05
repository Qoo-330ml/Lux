import { useQuery } from "@tanstack/react-query";
import { Maximize, Pause, Play, Settings2, Volume2 } from "lucide-react";
import { useCallback, useEffect, useRef, useState, type SyntheticEvent } from "react";
import { useParams, useSearchParams } from "react-router-dom";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { PlaybackEventState } from "../../lib/api/types";
import { imageUrl, mediaTitle } from "../home/media";

const TICKS_PER_SECOND = 10_000_000;
const PROGRESS_REPORT_INTERVAL_MS = 10_000;

export function PlayerPage() {
  const { itemId = "" } = useParams();
  const [searchParams] = useSearchParams();
  const [playing, setPlaying] = useState(false);
  const [failedStreamUrl, setFailedStreamUrl] = useState<string | null>(null);
  const requestedSourceId = searchParams.get("sourceId");
  const item = useQuery({ queryKey: queryKeys.item(itemId), queryFn: () => api.item(itemId), enabled: Boolean(itemId) });
  const playback = useQuery({ queryKey: queryKeys.playback(itemId), queryFn: () => api.playback(itemId), enabled: Boolean(itemId) });
  const videoRef = useRef<HTMLVideoElement>(null);
  const lastProgressReportRef = useRef(0);
  const hasStartedRef = useRef(false);
  const hasRestoredPositionRef = useRef(false);

  const reportPlayback = useCallback((state: PlaybackEventState, force = false, keepalive = false) => {
    const video = videoRef.current;
    if (!video || (state === "STOPPED" && !hasStartedRef.current)) return;
    const now = Date.now();
    if (!force && now - lastProgressReportRef.current < PROGRESS_REPORT_INTERVAL_MS) return;
    const positionTicks = Math.max(0, Math.round((Number.isFinite(video.currentTime) ? video.currentTime : 0) * TICKS_PER_SECOND));
    const durationTicks = Number.isFinite(video.duration) && video.duration >= 0
      ? Math.round(video.duration * TICKS_PER_SECOND)
      : null;
    lastProgressReportRef.current = now;
    void api.progress(itemId, positionTicks, durationTicks, state, keepalive).catch(() => undefined);
  }, [itemId]);

  useEffect(() => {
    lastProgressReportRef.current = 0;
    hasStartedRef.current = false;
    hasRestoredPositionRef.current = false;
  }, [itemId, requestedSourceId]);

  useEffect(() => {
    const handlePageHide = () => reportPlayback("STOPPED", true, true);
    window.addEventListener("pagehide", handlePageHide);
    return () => window.removeEventListener("pagehide", handlePageHide);
  }, [reportPlayback]);

  const restorePlaybackPosition = useCallback(() => {
    if (hasRestoredPositionRef.current) return;
    const video = videoRef.current;
    if (!video || !playback.data) return;
    if (video.readyState < 1 && !Number.isFinite(video.duration)) return;
    hasRestoredPositionRef.current = true;
    const resumeTicks = playback.data.positionTicks ?? 0;
    if (playback.data.isPlayed || resumeTicks <= 0) return;
    const resumeSeconds = resumeTicks / TICKS_PER_SECOND;
    if (!Number.isFinite(video.duration) || resumeSeconds < video.duration) {
      video.currentTime = resumeSeconds;
    }
  }, [playback.data]);

  useEffect(() => {
    restorePlaybackPosition();
  }, [restorePlaybackPosition]);

  if (item.isPending) return <section className="lux-page-state"><p>正在准备播放器…</p></section>;
  if (item.error) return <section className="lux-page-state"><h1>播放器加载失败</h1><p>{item.error.message}</p></section>;

  const media = item.data;
  const source = media.mediaSources?.find((entry) => entry.id === requestedSourceId)
    ?? media.mediaSources?.find((entry) => entry.isDefault)
    ?? media.mediaSources?.[0];
  const streamUrl = source?.sourceKind === "STRM_URL"
    ? source.externalUrl ?? ""
    : source
      ? `/api/v1/items/${encodeURIComponent(media.id)}/stream?sourceId=${encodeURIComponent(source.id)}`
      : "";
  const poster = imageUrl(media, "fanart") ?? imageUrl(media);
  const handleLoadedMetadata = () => {
    restorePlaybackPosition();
  };
  const handlePause = (event: SyntheticEvent<HTMLVideoElement>) => {
    setPlaying(false);
    if (!event.currentTarget.ended) reportPlayback("PAUSED", true);
  };

  return (
    <main className="lux-player-page">
      <div className="lux-player-topbar"><span>{mediaTitle(media)}</span><div><button type="button" aria-label="播放器设置"><Settings2 size={19} /></button><button type="button" aria-label="全屏"><Maximize size={19} /></button></div></div>
      <div className="lux-player-frame">
        {streamUrl ? (
          <video
            ref={videoRef}
            className="lux-video"
            src={streamUrl}
            poster={poster}
            controls
            preload="metadata"
            onError={() => setFailedStreamUrl(streamUrl)}
            onLoadedMetadata={handleLoadedMetadata}
            onPlay={() => { hasStartedRef.current = true; setPlaying(true); reportPlayback("PLAYING", true); }}
            onPause={handlePause}
            onTimeUpdate={() => reportPlayback("PLAYING")}
            onEnded={() => { setPlaying(false); reportPlayback("STOPPED", true); }}
            aria-label={`播放 ${mediaTitle(media)}`}
          />
        ) : null}
        {failedStreamUrl === streamUrl || !streamUrl ? <p className="lux-player-error" role="alert">浏览器无法播放这个媒体源。请尝试其他版本或使用支持该格式的客户端。</p> : null}
        <div className="lux-player-controls" aria-hidden="true"><button type="button"><Play size={17} fill="currentColor" /></button><div className="lux-player-progress"><span /></div><span>00:00</span><Volume2 size={17} /><button type="button"><Pause size={17} /></button></div>
        <span className="lux-player-status">{playing ? "正在播放" : "已暂停"}</span>
      </div>
    </main>
  );
}
