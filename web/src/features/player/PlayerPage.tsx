import { useQuery } from "@tanstack/react-query";
import { Maximize, Pause, Play, Settings2, Volume2 } from "lucide-react";
import { useEffect, useState } from "react";
import { useParams, useSearchParams } from "react-router-dom";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import { imageUrl, mediaTitle } from "../home/media";

export function PlayerPage() {
  const { itemId = "" } = useParams();
  const [searchParams] = useSearchParams();
  const [playing, setPlaying] = useState(false);
  const [playbackError, setPlaybackError] = useState(false);
  const requestedSourceId = searchParams.get("sourceId");
  useEffect(() => {
    setPlaybackError(false);
  }, [itemId, requestedSourceId]);
  const item = useQuery({ queryKey: queryKeys.item(itemId), queryFn: () => api.item(itemId), enabled: Boolean(itemId) });

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

  return (
    <main className="lux-player-page">
      <div className="lux-player-topbar"><span>{mediaTitle(media)}</span><div><button type="button" aria-label="播放器设置"><Settings2 size={19} /></button><button type="button" aria-label="全屏"><Maximize size={19} /></button></div></div>
      <div className="lux-player-frame">
        {streamUrl ? (
          <video className="lux-video" src={streamUrl} poster={poster} controls preload="metadata" onError={() => setPlaybackError(true)} onPlay={() => setPlaying(true)} onPause={() => setPlaying(false)} aria-label={`播放 ${mediaTitle(media)}`} />
        ) : null}
        {playbackError || !streamUrl ? <p className="lux-player-error" role="alert">浏览器无法播放这个媒体源。请尝试其他版本或使用支持该格式的客户端。</p> : null}
        <div className="lux-player-controls" aria-hidden="true"><button type="button"><Play size={17} fill="currentColor" /></button><div className="lux-player-progress"><span /></div><span>00:00</span><Volume2 size={17} /><button type="button"><Pause size={17} /></button></div>
        <span className="lux-player-status">{playing ? "正在播放" : "已暂停"}</span>
      </div>
    </main>
  );
}
