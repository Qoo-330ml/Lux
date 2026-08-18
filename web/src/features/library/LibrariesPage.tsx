import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import { api, type LibrarySortBy, type LibrarySortOrder } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { MediaItem, PageResponse } from "../../lib/api/types";
import { LibraryCard } from "../home/media";

export function LibrariesPage() {
  const queryClient = useQueryClient();
  const libraries = useQuery({ queryKey: queryKeys.libraries, queryFn: () => api.libraries() });

  if (libraries.isPending) return <section className="lux-page-state"><p>正在加载媒体库…</p></section>;
  if (libraries.error) return <section className="lux-page-state"><h1>媒体库加载失败</h1><p>{libraries.error.message}</p></section>;

  const items = libraries.data.libraries ?? [];
  const prefetchLibrary = (libraryId: string, kind: string) => {
    const itemTypes = kind === "SERIES" ? "SERIES" : kind === "MOVIE" ? "MOVIE" : kind === "MIXED" ? "MOVIE,SERIES" : undefined;
    const sortBy: LibrarySortBy = "Name";
    const sortOrder: LibrarySortOrder = "Ascending";
    void queryClient.prefetchInfiniteQuery({
      queryKey: queryKeys.library(libraryId, 1, itemTypes, sortBy, sortOrder, "all"),
      queryFn: ({ pageParam }) => api.libraryItems(libraryId, pageParam, itemTypes, { sortBy, sortOrder }),
      initialPageParam: 1,
      getNextPageParam: (lastPage: PageResponse<MediaItem>) => {
        const page = lastPage.page ?? 1;
        const pageSize = lastPage.pageSize ?? 24;
        const total = lastPage.total ?? 0;
        return page * pageSize < total ? page + 1 : undefined;
      },
    });
  };
  return (
    <section className="lux-page lux-page-narrow">
      <div className="lux-page-heading"><h1>媒体库</h1></div>
      <div className="lux-library-grid">
        {items.length ? items.map((library) => <LibraryCard key={library.id} library={library} onPrefetch={() => prefetchLibrary(library.id, library.kind)} />) : <div className="lux-empty-card"><span>当前账号没有可访问的媒体库。</span><Link to="/">返回首页</Link></div>}
      </div>
    </section>
  );
}
