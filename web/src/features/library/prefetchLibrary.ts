import type { QueryClient } from "@tanstack/react-query";
import { api, type LibrarySortBy, type LibrarySortOrder } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { Library, MediaItem, PageResponse } from "../../lib/api/types";

export function prefetchLibraryPage(queryClient: QueryClient, library: Library) {
  const itemTypes = library.kind === "SERIES"
    ? "SERIES"
    : library.kind === "MOVIE"
      ? "MOVIE"
      : library.kind === "MIXED"
        ? "MOVIE,SERIES"
        : undefined;
  const sortBy: LibrarySortBy = "Name";
  const sortOrder: LibrarySortOrder = "Ascending";
  return queryClient.prefetchInfiniteQuery({
    queryKey: queryKeys.library(library.id, 1, itemTypes, sortBy, sortOrder, "all"),
    queryFn: ({ pageParam }) => api.libraryItems(library.id, pageParam, itemTypes, { sortBy, sortOrder }),
    initialPageParam: 1,
    getNextPageParam: (lastPage: PageResponse<MediaItem>) => {
      const page = lastPage.page ?? 1;
      const pageSize = lastPage.pageSize ?? 24;
      const total = lastPage.total ?? 0;
      return page * pageSize < total ? page + 1 : undefined;
    },
  });
}
