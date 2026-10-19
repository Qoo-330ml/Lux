import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { api } from "../../lib/api/client";
import { queryKeys, queryRefreshIntervals } from "../../lib/api/query-keys";
import type { MediaItem } from "../../lib/api/types";
import { useSseConnectionState } from "../activity/useUserEvents";

export function useMediaEpisodes(item: MediaItem) {
  const isSeries = item.itemType === "SERIES";
  const isSeason = item.itemType === "SEASON";
  const isEpisode = item.itemType === "EPISODE";
  const sseState = useSseConnectionState();
  const refetchInterval = sseState === "connected" ? false : queryRefreshIntervals.mediaSurface;
  const seasons = useQuery({
    queryKey: queryKeys.children(item.id, "SEASON"),
    queryFn: () => api.children(item.id, { itemType: "SEASON" }),
    enabled: isSeries,
    refetchInterval,
    refetchIntervalInBackground: false,
  });
  const activeSeasonId = seasons.data?.items?.[0]?.id;
  const singleSeason = seasons.data?.items?.length === 1 ? seasons.data.items[0] : undefined;
  const hierarchySeriesId = !isSeries ? item.seriesId ?? item.parentId ?? undefined : undefined;
  const episodeSeriesId = isSeries ? item.id : hierarchySeriesId;
  const episodeSeasonId = isSeries
    ? activeSeasonId
    : isSeason
      ? item.id
      : isEpisode
        ? item.parentId ?? undefined
        : undefined;
  const episodes = useInfiniteQuery({
    queryKey: queryKeys.children(episodeSeriesId ?? item.id, "EPISODE", episodeSeasonId),
    queryFn: ({ pageParam }) => api.children(episodeSeriesId ?? item.id, {
      page: pageParam,
      itemType: "EPISODE",
      seasonId: episodeSeasonId,
    }),
    initialPageParam: 1,
    getNextPageParam: (lastPage) => {
      const page = lastPage.page ?? 1;
      const pageSize = lastPage.pageSize ?? 100;
      const total = lastPage.total ?? 0;
      return page * pageSize < total ? page + 1 : undefined;
    },
    enabled: Boolean(episodeSeriesId) && Boolean(episodeSeasonId) && Boolean(isSeries || isSeason || isEpisode),
    refetchInterval,
    refetchIntervalInBackground: false,
  });
  const episodeItems = episodes.data?.pages.flatMap((page) => page.items ?? []) ?? [];
  const episodeQueryEnabled = Boolean(episodeSeriesId && episodeSeasonId && (isSeries || isSeason || isEpisode));
  const pending = (isSeries && seasons.isPending) || (episodes.isPending && episodeQueryEnabled);
  return { seasons, singleSeason, episodes, episodeItems, pending };
}
