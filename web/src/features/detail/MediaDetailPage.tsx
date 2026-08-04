import { useQuery } from "@tanstack/react-query";
import { Heart, Play, Radio, Sparkles } from "lucide-react";
import { Link, useParams } from "react-router-dom";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import { imageUrl, mediaTitle, runtimeLabel } from "../home/media";

export function MediaDetailPage() {
  const { itemId = "" } = useParams();
  const item = useQuery({ queryKey: queryKeys.item(itemId), queryFn: () => api.item(itemId), enabled: Boolean(itemId) });
  const playback = useQuery({ queryKey: queryKeys.playback(itemId), queryFn: () => api.playback(itemId), enabled: Boolean(itemId) });

  if (item.isPending) return <section className="lux-page-state"><p>正在加载媒体详情…</p></section>;
  if (item.error) return <section className="lux-page-state"><h1>媒体详情加载失败</h1><p>{item.error.message}</p></section>;

  const media = item.data;
  const backdrop = imageUrl(media, "fanart") ?? imageUrl(media);
  const poster = imageUrl(media);
  const source = media.mediaSources?.find((entry) => entry.isDefault) ?? media.mediaSources?.[0];

  return (
    <article className="lux-detail-page">
      {backdrop ? <img className="lux-detail-backdrop" src={backdrop} alt="" /> : null}
      <div className="lux-detail-overlay" />
      <div className="lux-detail-content">
        <div className="lux-detail-grid">
          <div className="lux-detail-poster">{poster ? <img src={poster} alt={`${mediaTitle(media)} 海报`} /> : <span><Sparkles size={32} />{mediaTitle(media)}</span>}</div>
          <div className="lux-detail-copy">
            <span className="lux-eyebrow">{media.itemType === "SERIES" ? "SERIES" : "MOVIE"}</span>
            <h1>{mediaTitle(media)}</h1>
            <div className="lux-detail-meta">
              {media.productionYear ? <span>{media.productionYear}</span> : null}
              {runtimeLabel(media.runtimeTicks) ? <span>{runtimeLabel(media.runtimeTicks)}</span> : null}
              {source?.qualityLabel ? <span>{source.qualityLabel}</span> : null}
              {playback.data?.isPlayed ? <span>已观看</span> : null}
            </div>
            <p className="lux-detail-overview">{media.overview || "暂无简介。"}</p>
            <div className="lux-hero-actions">
              <Link className="lux-button lux-button-primary" to={`/watch/${media.id}`}><Play size={17} fill="currentColor" /> 播放</Link>
              <button className="lux-button lux-button-glass" type="button"><Heart size={17} /> {playback.data?.isFavorite ? "已收藏" : "收藏"}</button>
              <span className="lux-detail-source"><Radio size={16} /> {source ? source.container || "DIRECT PLAY" : "暂无可播放版本"}</span>
            </div>
          </div>
        </div>
      </div>
    </article>
  );
}
