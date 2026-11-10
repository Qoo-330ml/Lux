import { useQuery } from "@tanstack/react-query";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { MediaItem, MetadataRefreshMode } from "../../lib/api/types";

const ADMIN_STRATEGY_STALE_TIME = 5 * 60 * 1000;
const ITEM_TYPES_WITHOUT_REFRESH = new Set(["VIDEO", "FOLDER"]);

/**
 * Effective metadata refresh mode for a single item: the item's own library
 * override, then the global media strategy, then FILL_MISSING.
 *
 * The two strategy reads are admin endpoints, so they only run for a server
 * manager and only when the caller can actually show the refresh action. They
 * reuse the admin query keys, which means every card on a page shares one
 * request instead of starting its own. While a read needed for the resolution is
 * pending or failed the mode is undefined; callers must keep the neutral label
 * and the old FILL_MISSING request instead of claiming a mode they have not
 * resolved.
 */
export function useItemMetadataRefreshMode(
  item: Pick<MediaItem, "libraryId" | "itemType">,
  enabled = true,
): MetadataRefreshMode | undefined {
  // Reuse the session already cached by the app shell; never start a /auth/me
  // request from a card, and never touch the admin endpoints for a non-admin.
  const session = useQuery({
    queryKey: queryKeys.me,
    queryFn: () => api.me(),
    enabled: false,
  });
  const canManageServer = session.data?.user?.canManageServer === true;
  const queryEnabled = enabled
    && canManageServer
    && !ITEM_TYPES_WITHOUT_REFRESH.has(item.itemType ?? "");
  const libraries = useQuery({
    queryKey: queryKeys.adminLibraries,
    queryFn: () => api.adminLibraries(),
    enabled: queryEnabled && Boolean(item.libraryId),
    staleTime: ADMIN_STRATEGY_STALE_TIME,
    retry: false,
  });
  const settings = useQuery({
    queryKey: queryKeys.adminSettings,
    queryFn: () => api.adminSettings(),
    enabled: queryEnabled,
    staleTime: ADMIN_STRATEGY_STALE_TIME,
    retry: false,
  });

  if (!queryEnabled) return undefined;
  if (item.libraryId) {
    // A failed library read leaves the override unknown, so do not fall back
    // to the global mode and claim a resolution that was never established.
    if (libraries.isError || !libraries.data) return undefined;
    const libraryMode = libraries.data.libraries
      ?.find((library) => library.id === item.libraryId)
      ?.mediaStrategy?.metadataRefreshMode;
    if (libraryMode) return libraryMode;
  }
  if (settings.isError || !settings.data) return undefined;
  return settings.data.mediaStrategy?.metadataRefreshMode ?? "FILL_MISSING";
}
