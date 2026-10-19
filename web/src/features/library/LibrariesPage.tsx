import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import { GLOBAL_COLLECTION_LIBRARY_ID } from "../../lib/api/types";
import { LibraryCard } from "../home/media";
import { prefetchLibraryPage } from "./prefetchLibrary";

export function LibrariesPage() {
  const queryClient = useQueryClient();
  const libraries = useQuery({ queryKey: queryKeys.libraries, queryFn: () => api.libraries() });
  const items = libraries.data?.libraries ?? [];
  const hasCollectionLibrary = items.some((library) => library.id === GLOBAL_COLLECTION_LIBRARY_ID);
  const collectionLatest = useQuery({
    queryKey: queryKeys.homeLatestLibraries([GLOBAL_COLLECTION_LIBRARY_ID]),
    queryFn: ({ signal }) => api.homeLibrariesLatest([GLOBAL_COLLECTION_LIBRARY_ID], signal),
    enabled: hasCollectionLibrary,
    staleTime: 30_000,
    retry: false,
  });

  if (libraries.isPending) return <section className="lux-page-state"><p>正在加载媒体库…</p></section>;
  if (libraries.error) return <section className="lux-page-state"><h1>媒体库加载失败</h1><p>{libraries.error.message}</p></section>;

  const collectionCoverItems = collectionLatest.data?.libraries.find((library) => library.libraryId === GLOBAL_COLLECTION_LIBRARY_ID)?.items;
  return (
    <section className="lux-page lux-page-narrow">
      <div className="lux-page-heading"><h1>媒体库</h1></div>
      <div className="lux-library-grid">
        {items.length ? items.map((library) => <LibraryCard
          key={library.id}
          library={library}
          coverItems={library.id === GLOBAL_COLLECTION_LIBRARY_ID ? collectionCoverItems : undefined}
          onPrefetch={() => void prefetchLibraryPage(queryClient, library)}
        />) : <div className="lux-empty-card"><span>当前账号没有可访问的媒体库。</span><Link to="/">返回首页</Link></div>}
      </div>
    </section>
  );
}
