import { Play, Star } from "lucide-react";
import { Link } from "react-router-dom";
import type { Library, MediaItem } from "../../lib/api/types";

export function mediaTitle(item: MediaItem) {
  return item.title || item.name || "未命名媒体";
}

export function imageUrl(item: MediaItem, type: "poster" | "fanart" = "poster") {
  if (!item.imageTags?.[type]) return undefined;
  return `/api/v1/items/${encodeURIComponent(item.id)}/images/${type}`;
}

export function runtimeLabel(ticks?: number | null) {
  if (!ticks) return undefined;
  const minutes = Math.round(ticks / 10_000_000 / 60);
  if (!minutes) return undefined;
  const hours = Math.floor(minutes / 60);
  return hours ? `${hours}h ${minutes % 60}m` : `${minutes}m`;
}

export function MediaCard({ item, landscape = false }: { item: MediaItem; landscape?: boolean }) {
  const image = imageUrl(item, landscape ? "fanart" : "poster") ?? imageUrl(item);
  const progress = item.userData?.playbackPositionTicks && item.runtimeTicks
    ? Math.min(100, Math.round((item.userData.playbackPositionTicks / item.runtimeTicks) * 100))
    : 0;

  return (
    <Link className={landscape ? "lux-media-card lux-media-card-landscape" : "lux-media-card"} to={`/items/${item.id}`}>
      <div className="lux-media-art">
        {image ? <img src={image} alt="" loading="lazy" /> : <div className="lux-media-placeholder">{mediaTitle(item)}</div>}
        <span className="lux-media-hover-play" aria-hidden="true"><Play size={22} fill="currentColor" /></span>
        {progress > 0 && progress < 90 ? <span className="lux-progress"><span style={{ width: `${progress}%` }} /></span> : null}
      </div>
      <div className="lux-media-copy">
        <strong>{mediaTitle(item)}</strong>
        <span>{[item.productionYear, item.itemType === "SERIES" ? "剧集" : "电影"].filter(Boolean).join(" · ")}</span>
      </div>
    </Link>
  );
}

export function MediaRail({ title, items, landscape = false }: { title: string; items: MediaItem[]; landscape?: boolean }) {
  if (!items.length) return null;
  return (
    <section className="lux-section" aria-label={title}>
      <div className="lux-section-heading"><h2>{title}</h2><span>{items.length} 项</span></div>
      <div className={landscape ? "lux-media-rail lux-media-rail-landscape" : "lux-media-rail"}>
        {items.map((item) => <MediaCard item={item} landscape={landscape} key={item.id} />)}
      </div>
    </section>
  );
}

export function LibraryCard({ library }: { library: Library }) {
  const label = library.kind === "SERIES" ? "电视剧" : library.kind === "MOVIE" ? "电影" : "媒体库";
  return (
    <Link className="lux-library-card" to={`/libraries/${library.id}`}>
      {library.coverImageUrl ? <img className="lux-library-cover" src={library.coverImageUrl} alt="" loading="lazy" /> : <span className="lux-library-icon" aria-hidden="true">{library.kind === "SERIES" ? "▣" : "◈"}</span>}
      <span><small>{label}</small><strong>{library.name}</strong></span>
    </Link>
  );
}

export function Rating({ value }: { value?: number | null }) {
  return value ? <span className="lux-rating"><Star size={15} fill="currentColor" /> {value.toFixed(1)}</span> : null;
}
