import { memo } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "../../lib/api/client";
import { queryKeys, queryRefreshIntervals } from "../../lib/api/query-keys";
import type { MediaActor, MediaItem, MediaSource } from "../../lib/api/types";
import { MediaCast } from "./MediaCast";
import { MediaNfoPanel } from "./MediaNfoPanel";
import { MediaStills } from "./MediaStills";

const emptyActors: MediaActor[] = [];

export const MediaCastSection = memo(function MediaCastSection({ actors }: { actors?: MediaActor[] | null }) {
  return <MediaCast actors={actors ?? emptyActors} />;
});

export const MediaStillsSection = memo(function MediaStillsSection({ itemId, title }: { itemId: string; title: string }) {
  const images = useQuery({
    queryKey: queryKeys.itemImages(itemId),
    queryFn: () => api.itemImages(itemId),
    refetchInterval: queryRefreshIntervals.mediaSurface,
  });
  return <MediaStills images={images.data?.images ?? []} title={title} />;
});

export const MediaNfoSection = memo(function MediaNfoSection({ item, source, detailTitle, addedAtLabel }: {
  item: MediaItem;
  source?: MediaSource;
  detailTitle: string;
  addedAtLabel?: string;
}) {
  const originalTitle = item.originalTitle?.trim();
  return <MediaNfoPanel
    details={item.nfo}
    originalTitle={originalTitle !== detailTitle ? originalTitle : undefined}
    productionYear={item.productionYear}
    addedAtLabel={addedAtLabel}
    mediaInfo={source ? {
      source,
      itemType: item.itemType,
      lastAirDate: item.lastAirDate,
      status: item.status ?? item.nfo?.status,
      originalLanguage: item.originalLanguage ?? item.nfo?.originalLanguage,
    } : undefined}
  />;
});
