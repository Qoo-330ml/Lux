import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { Link, useParams } from "react-router-dom";
import { useEffect, useRef } from "react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { Library } from "../../lib/api/types";
import { MediaCard } from "../home/media";

export function libraryItemTypeFilter(kind?: Library["kind"]) {
  if (kind === "SERIES") return "SERIES";
  if (kind === "MOVIE") return "MOVIE";
  if (kind === "MIXED") return "MOVIE,SERIES";
  return undefined;
}

export function LibraryPage() {
  const { libraryId = "" } = useParams();
  const libraries = useQuery({ queryKey: queryKeys.libraries, queryFn: () => api.libraries() });
  const library = libraries.data?.libraries?.find((entry) => entry.id === libraryId);
  const itemTypes = libraryItemTypeFilter(library?.kind);
  const pages = useInfiniteQuery({
    queryKey: queryKeys.library(libraryId, 1, itemTypes),
    queryFn: ({ pageParam }) => api.libraryItems(libraryId, pageParam, itemTypes),
    initialPageParam: 1,
    enabled: Boolean(libraryId && library),
    getNextPageParam: (lastPage) => {
      const page = lastPage.page ?? 1;
      const pageSize = lastPage.pageSize ?? 24;
      const total = lastPage.total ?? 0;
      return page * pageSize < total ? page + 1 : undefined;
    },
  });
  const loadMoreRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    const sentinel = loadMoreRef.current;
    if (!sentinel || !pages.hasNextPage || pages.isFetchingNextPage || typeof IntersectionObserver === "undefined") return;
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) void pages.fetchNextPage();
    }, { rootMargin: "600px 0px" });
    observer.observe(sentinel);
    return () => observer.disconnect();
  }, [pages.fetchNextPage, pages.hasNextPage, pages.isFetchingNextPage]);

  if (libraries.isPending) return <section className="lux-page-state"><p>正在整理媒体库…</p></section>;
  if (pages.error && !pages.data) return <section className="lux-page-state"><h1>媒体库加载失败</h1><p>{pages.error.message}</p></section>;
  if (!library) return <section className="lux-page-state"><h1>媒体库不存在</h1><p>这个媒体库可能已被删除或你没有访问权限。</p></section>;
  if (pages.isPending) return <section className="lux-page-state"><p>正在整理媒体库…</p></section>;

  const loadedItems = pages.data?.pages.flatMap((page) => page.items ?? []) ?? [];
  const total = pages.data?.pages[0]?.total ?? 0;

  return (
    <section className="lux-page lux-page-narrow">
      <div className="lux-page-heading"><span className="lux-eyebrow">LIBRARY</span><h1>{library?.name || "媒体库"}</h1><p>{total} 项内容</p></div>
      <div className="lux-poster-grid">
        {loadedItems.map((item) => <MediaCard item={item} compactRating key={item.id} />)}
      </div>
      {!loadedItems.length ? <div className="lux-empty-card"><span>这个媒体库还没有内容。</span><Link to="/libraries">返回媒体库</Link></div> : null}
      <div ref={loadMoreRef} aria-hidden="true" />
      {pages.isFetchingNextPage ? <p className="lux-muted-copy" role="status">正在加载更多…</p> : null}
      {pages.isFetchNextPageError ? (
        <p className="lux-error-copy" role="alert">
          加载更多失败：{pages.error?.message || "请稍后重试"}
          <button className="lux-button lux-button-secondary" type="button" onClick={() => void pages.fetchNextPage()}>重试</button>
        </p>
      ) : null}
      {!pages.hasNextPage && loadedItems.length ? <p className="lux-muted-copy" role="status">已加载全部 {total} 项</p> : null}
    </section>
  );
}
