import { Play } from "lucide-react";
import { memo, type ReactNode } from "react";
import { Link } from "react-router-dom";
import type { MediaItem } from "../../lib/api/types";
import { EpisodeCount, MediaPlaceholder, Rating, imageUrl, mediaTitle, mediaTypeLabel, playbackProgress } from "./media-display";
import { useImageVariants } from "./image-variants";

type MediaCardLinkProps = { item: MediaItem; href?: string; children: ReactNode };

function MediaCardLink({ item, href, children }: MediaCardLinkProps) {
  const isFolder = item.itemType === "FOLDER";
  if (isFolder && !href) return <div className="lux-media-card-link">{children}</div>;
  return (
    <Link className="lux-media-card-link" to={href ?? `/items/${item.id}`} aria-label={isFolder ? `打开文件夹 ${mediaTitle(item)}` : `查看 ${mediaTitle(item)} 详情`}>
      {children}
    </Link>
  );
}

export const MediaCardVisual = memo(function MediaCardVisual({ item, landscape, href }: { item: MediaItem; landscape: boolean; href?: string }) {
  const { enabled } = useImageVariants();
  const image = imageUrl(item, landscape ? "fanart" : "poster", enabled ? 360 : undefined)
    ?? imageUrl(item, "poster", enabled ? 360 : undefined);
  const progress = playbackProgress(item);
  const isFolder = item.itemType === "FOLDER";
  return (
    <MediaCardLink item={item} href={href}>
      <div className="lux-media-art">
        {image ? <img src={image} alt="" loading="lazy" decoding="async" /> : <MediaPlaceholder item={item} />}
        {!isFolder ? <Rating value={item.rating} placement="card" /> : null}
        <EpisodeCount item={item} />
        {!isFolder ? <span className="lux-media-hover-play" aria-hidden="true"><Play size={22} fill="currentColor" /></span> : null}
        {!isFolder && progress > 0 && progress < 90 ? <span className="lux-progress"><span style={{ width: `${progress}%` }} /></span> : null}
      </div>
    </MediaCardLink>
  );
});

export const MediaCardCopy = memo(function MediaCardCopy({ item, metadataAttention, href }: { item: MediaItem; metadataAttention: boolean; href?: string }) {
  return (
    <MediaCardLink item={item} href={href}>
      <div className="lux-media-copy">
        <strong>{mediaTitle(item)}</strong>
        <span>{[item.productionYear, mediaTypeLabel(item.itemType)].filter(Boolean).join(" · ")}</span>
        {metadataAttention && item.itemType !== "FOLDER" ? <span className="lux-metadata-attention-badge">待确认</span> : null}
      </div>
    </MediaCardLink>
  );
});
