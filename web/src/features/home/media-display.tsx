import type { MediaItem } from "../../lib/api/types";

export function mediaTitle(item: MediaItem) {
  return item.title || item.name || "未命名媒体";
}

export function episodeTitle(
  item: MediaItem,
  seasonNumber?: number | null,
  fallbackEpisodeNumber?: number,
) {
  const season = paddedEpisodeNumber(seasonNumber ?? item.parentIndexNumber);
  const episode = paddedEpisodeNumber(item.indexNumber ?? fallbackEpisodeNumber);
  if (item.itemType !== "EPISODE" || season == null || episode == null) {
    return mediaTitle(item);
  }
  return `S${season}E${episode} · ${mediaTitle(item)}`;
}

function paddedEpisodeNumber(value?: number | null) {
  return value != null && Number.isInteger(value) && value >= 0
    ? String(value).padStart(2, "0")
    : undefined;
}

export function mediaTypeLabel(itemType?: string | null) {
  switch (itemType) {
    case "MOVIE": return "电影";
    case "SERIES": return "剧集";
    case "SEASON": return "季度";
    case "EPISODE": return "单集";
    case "BOX_SET": return "合集";
    case "VIDEO": return "其他视频";
    case "FOLDER": return "文件夹";
    default: return "媒体";
  }
}

export function imageUrl(item: MediaItem, type: "poster" | "fanart" | "thumb" | "logo" = "poster") {
  const resolvedType = item.imageTags?.[type]
    ? type
    : type === "fanart" && item.imageTags?.thumb
      ? "thumb"
      : undefined;
  if (!resolvedType) return undefined;
  const tag = item.imageTags?.[resolvedType];
  const imageItemId = resolvedType === "poster" ? item.posterItemId ?? item.id : item.id;
  const path = `/api/v1/items/${encodeURIComponent(imageItemId)}/images/${resolvedType}`;
  return tag ? `${path}?tag=${encodeURIComponent(tag)}` : path;
}

export function posterUrlWithFallback(item: MediaItem, series?: MediaItem) {
  return imageUrl(item)
    ?? (item.itemType === "SEASON" && series?.itemType === "SERIES" ? imageUrl(series) : undefined);
}

export function runtimeLabel(ticks?: number | null) {
  if (!ticks) return undefined;
  const minutes = Math.round(ticks / 10_000_000 / 60);
  if (!minutes) return undefined;
  const hours = Math.floor(minutes / 60);
  return hours ? `${hours}h ${minutes % 60}m` : `${minutes}m`;
}

export function EpisodeCount({ item }: { item: MediaItem }) {
  if (item.itemType === "BOX_SET" && item.memberCount != null && Number.isFinite(item.memberCount) && item.memberCount >= 0) {
    const label = `${item.memberCount} 部`;
    return <span className="lux-media-episode-count" aria-label={label} title={label}>{label}</span>;
  }
  if (item.itemType !== "SERIES" && item.itemType !== "SEASON") return null;
  if (item.episodeCount == null || !Number.isFinite(item.episodeCount) || item.episodeCount < 0) return null;
  const label = `${item.episodeCount} 集`;
  return <span className="lux-media-episode-count" aria-label={label} title={label}>{label}</span>;
}

export function Rating({ value, placement = "default" }: { value?: number | null; placement?: "default" | "card" }) {
  if (value == null || !Number.isFinite(value) || value < 0 || value > 10) return null;
  const score = value.toFixed(1);
  return (
    <span className={["lux-rating", placement === "card" ? "lux-card-rating" : ""].filter(Boolean).join(" ")} aria-label={`评分 ${score}`} title={`评分 ${score}`}>
      <strong>{score}</strong>
    </span>
  );
}
