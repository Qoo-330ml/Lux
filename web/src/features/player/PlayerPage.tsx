import { useQuery } from "@tanstack/react-query";
import { ArrowLeft, Maximize, Pause, Play, Settings2, Volume2 } from "lucide-react";
import { useState } from "react";
import { Link, useParams } from "react-router-dom";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import { imageUrl, mediaTitle } from "../home/media";

export function PlayerPage() {
  const { itemId = "" } = useParams();
  const [playing, setPlaying] = useState(false);
  const item = useQuery({ queryKey: queryKeys.item(itemId), queryFn: () => api.item(itemId), enabled: Boolean(itemId) });

  if (item.isPending) return <section className="lux-page-state"><p>正在准备播放器…</p></section>;
  if (item.error) return <section className="lux-page-state"><h1>播放器加载失败</h1><p>{item.error.message}</p></section>;

  const media = item.data;
  const source = media.mediaSources?.find((entry) => entry.isDefault) ?? media.mediaSources?.[0];
  const sourceQuery = source ? `?sourceId=${encodeURIComponent(source.id)}` : "";
  const streamUrl = `/api/v1/items/${encodeURIComponent(media.id)}/stream${sourceQuery}`;
  const poster = imageUrl(media, "fanart") ?? imageUrl(media);

  return (
    <main className="lux-player-page">
      <div className="lux-player-topbar"><Link to={`/items/${media.id}`} aria-label="返回媒体详情"><ArrowLeft size={20} /></Link><span>{mediaTitle(media)}</span><div><button type="button" aria-label="播放器设置"><Settings2 size={19} /></button><button type="button" aria-label="全屏"><Maximize size={19} /></button></div></div>
      <div className="lux-player-frame">
        <video className="lux-video" src={streamUrl} poster={poster} controls preload="metadata" onPlay={() => setPlaying(true)} onPause={() => setPlaying(false)} aria-label={`播放 ${mediaTitle(media)}`} />
        <div className="lux-player-controls" aria-hidden="true"><button type="button"><Play size={17} fill="currentColor" /></button><div className="lux-player-progress"><span /></div><span>00:00</span><Volume2 size={17} /><button type="button"><Pause size={17} /></button></div>
        <span className="lux-player-status">{playing ? "正在播放" : "已暂停"}</span>
      </div>
    </main>
  );
}
