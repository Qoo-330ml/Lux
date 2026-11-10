import { useQuery } from "@tanstack/react-query";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { ItemImagesResponse, MediaItem } from "../../lib/api/types";
import { imageUrl } from "../home/media";

/** The logo belongs to the series for episodes, so every episode of a show shows the same logo. */
export function titleLogoOwnerId(media: MediaItem | null | undefined): string | null {
  if (!media) return null;
  if (media.itemType === "EPISODE") return media.seriesId ?? media.id;
  return media.id;
}

export function selectLogoUrl(response: ItemImagesResponse): string | null {
  return response.images?.find((image) => image.imageType.toUpperCase() === "LOGO")?.url ?? null;
}

/**
 * Title logo shown in the player top bar, or null when the item has none (the top bar then shows
 * the text title). Same lookup as the detail page: the images listing first, then the item's tag.
 */
export function usePlayerTitleLogo(media: MediaItem | null | undefined): string | null {
  const ownerId = titleLogoOwnerId(media);
  const query = useQuery({
    queryKey: queryKeys.itemImages(ownerId ?? ""),
    queryFn: () => api.itemImages(ownerId ?? ""),
    enabled: ownerId !== null,
    select: selectLogoUrl,
    staleTime: 5 * 60_000,
    retry: false,
  });
  const ownTag = media && ownerId === media.id ? imageUrl(media, "logo") : undefined;
  return query.data ?? ownTag ?? null;
}
