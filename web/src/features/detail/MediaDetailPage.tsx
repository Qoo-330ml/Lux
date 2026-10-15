import { useQuery } from "@tanstack/react-query";
import { useParams, useSearchParams } from "react-router-dom";
import { api } from "../../lib/api/client";
import { queryKeys, queryRefreshIntervals } from "../../lib/api/query-keys";
import type { MediaItem } from "../../lib/api/types";
import { MediaSourcePanel, MediaSourceProvider, useMediaSource } from "./MediaSourcePanel";
import { MediaCastSection, MediaNfoSection, MediaStillsSection } from "./MediaDetailSections";
import { mediaTitle } from "../home/media-display";
import { MediaDetailActions, MediaDetailActionControls, MediaDetailMobileHeader } from "./MediaDetailActions";
import { MediaEpisodesSection } from "./MediaEpisodesSection";
import { MediaDetailArtwork, MediaDetailHero } from "./MediaDetailHero";

export function MediaDetailPage() {
  const { itemId = "" } = useParams();
  const [routeParams] = useSearchParams();
  const item = useQuery({
    queryKey: queryKeys.item(itemId),
    queryFn: () => api.item(itemId),
    enabled: Boolean(itemId),
    refetchInterval: queryRefreshIntervals.mediaSurface,
  });
  if (item.isPending) return <section className="lux-page-state"><p>正在加载媒体详情…</p></section>;
  if (item.error) return <section className="lux-page-state"><h1>媒体详情加载失败</h1><p>{item.error.message}</p></section>;
  return <MediaSourceProvider key={itemId} item={item.data} requestedSourceId={routeParams.get("sourceId") ?? undefined}>
    <MediaDetailActions item={item.data} pendingReview={routeParams.get("metadataStatus")?.toUpperCase() === "PENDING"}>
      <MediaDetailContent media={item.data} />
    </MediaDetailActions>
  </MediaSourceProvider>;
}

function MediaDetailContent({ media }: { media: MediaItem }) {
  const [searchParams] = useSearchParams();
  const collectionId = searchParams.get("collectionId") ?? undefined;
  const { source } = useMediaSource();
  const detailKind = ({ VIDEO: "video", SERIES: "series", SEASON: "season", EPISODE: "episode" } as Record<string, string>)[media.itemType ?? ""] ?? "movie";
  const showStills = media.itemType === "MOVIE" || media.itemType === "SERIES";
  const mediaTagline = media.nfo?.tagline?.trim();

  return (
    <article className={`lux-detail-page lux-detail-page-${detailKind}`}>
      <MediaDetailMobileHeader />
      <MediaDetailArtwork media={media} placement="backdrop" />
      <div className="lux-detail-overlay" />
      <div className="lux-detail-content">
        <div className="lux-detail-grid">
          <MediaDetailArtwork media={media} placement="poster" />
          <div className="lux-detail-copy">
            <MediaDetailHero media={media} collectionId={collectionId} />
            <MediaDetailActionControls />
            <MediaSourcePanel />
          </div>
        </div>
        <div className="lux-detail-sections">
          <MediaEpisodesSection item={media}>
          {mediaTagline ? (
            <div className="lux-detail-tagline-wrap">
              <div className="lux-detail-tagline lux-media-nfo-tagline font-serif">
                <span className="lux-tagline-quote" aria-hidden="true">“</span>
                <span className="lux-tagline-text">{mediaTagline}</span>
                <span className="lux-tagline-quote" aria-hidden="true">”</span>
              </div>
            </div>
          ) : null}
          <div className="lux-detail-cast">
            <MediaCastSection actors={media.actors} />
          </div>
          </MediaEpisodesSection>
          {showStills ? <MediaStillsSection itemId={media.id} title={mediaTitle(media)} /> : null}
          <div className="lux-detail-nfo">
            <MediaNfoSection item={media} source={source} />
          </div>
        </div>
      </div>
    </article>
  );
}
