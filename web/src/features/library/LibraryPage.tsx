import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { Link, useParams } from "react-router-dom";
import { useEffect, useRef, useState } from "react";
import { LuxSelect } from "../../components/LuxSelect";
import { api, type LibrarySortBy, type LibrarySortOrder } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { Library } from "../../lib/api/types";
import { MediaCard } from "../home/media";

const LIBRARY_SORT_STORAGE_KEY = "lux.library.sort";

type LibrarySortPreference = {
  sortBy: LibrarySortBy;
  sortOrder: LibrarySortOrder;
};

const DEFAULT_LIBRARY_SORT_PREFERENCE: LibrarySortPreference = {
  sortBy: "Name",
  sortOrder: "Ascending",
};

export function libraryItemTypeFilter(kind?: Library["kind"]) {
  if (kind === "SERIES") return "SERIES";
  if (kind === "MOVIE") return "MOVIE";
  if (kind === "MIXED") return "MOVIE,SERIES";
  return undefined;
}

function readLibrarySortPreference(libraryId: string): LibrarySortPreference {
  const storage = getStorage();
  if (!storage) return DEFAULT_LIBRARY_SORT_PREFERENCE;

  try {
    const stored = JSON.parse(storage.getItem(librarySortStorageKey(libraryId)) ?? "null") as Partial<LibrarySortPreference> | null;
    return {
      sortBy: isLibrarySortBy(stored?.sortBy) ? stored.sortBy : DEFAULT_LIBRARY_SORT_PREFERENCE.sortBy,
      sortOrder: isLibrarySortOrder(stored?.sortOrder) ? stored.sortOrder : DEFAULT_LIBRARY_SORT_PREFERENCE.sortOrder,
    };
  } catch {
    return DEFAULT_LIBRARY_SORT_PREFERENCE;
  }
}

function saveLibrarySortPreference(libraryId: string, preference: LibrarySortPreference): void {
  const storage = getStorage();
  if (!storage) return;

  try {
    storage.setItem(librarySortStorageKey(libraryId), JSON.stringify(preference));
  } catch {
    // Sort preferences are best-effort when browser storage is unavailable or full.
  }
}

function librarySortStorageKey(libraryId: string): string {
  return `${LIBRARY_SORT_STORAGE_KEY}:${encodeURIComponent(libraryId)}`;
}

function isLibrarySortBy(value: unknown): value is LibrarySortBy {
  return value === "Name" || value === "DateCreated" || value === "PremiereDate" || value === "CommunityRating";
}

function isLibrarySortOrder(value: unknown): value is LibrarySortOrder {
  return value === "Ascending" || value === "Descending";
}

function getStorage(): Storage | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

export function LibraryPage() {
  const { libraryId = "" } = useParams();
  const [sortState, setSortState] = useState(() => ({
    libraryId,
    preference: readLibrarySortPreference(libraryId),
  }));
  const sortPreference = sortState.libraryId === libraryId ? sortState.preference : readLibrarySortPreference(libraryId);
  const { sortBy, sortOrder } = sortPreference;
  const libraries = useQuery({ queryKey: queryKeys.libraries, queryFn: () => api.libraries() });
  const library = libraries.data?.libraries?.find((entry) => entry.id === libraryId);
  const itemTypes = libraryItemTypeFilter(library?.kind);

  useEffect(() => {
    if (typeof document === "undefined") return;

    const libraryName = library?.name.trim();
    document.title = libraryName ? `${libraryName} - Lux` : "Lux";
    return () => {
      document.title = "Lux";
    };
  }, [library?.name]);

  const pages = useInfiniteQuery({
    queryKey: queryKeys.library(libraryId, 1, itemTypes, sortBy, sortOrder),
    queryFn: ({ pageParam }) => api.libraryItems(libraryId, pageParam, itemTypes, { sortBy, sortOrder }),
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
  const sortOptions = [
    { value: "Name", label: "标题" },
    { value: "DateCreated", label: "最近添加" },
    { value: "PremiereDate", label: "发行日期" },
    { value: "CommunityRating", label: "评分" },
  ] as const;
  const ascendingLabel = sortBy === "Name" ? "A → Z" : sortBy === "CommunityRating" ? "从低到高" : "从旧到新";
  const descendingLabel = sortBy === "Name" ? "Z → A" : sortBy === "CommunityRating" ? "从高到低" : "从新到旧";
  const orderOptions = [
    { value: "Ascending", label: ascendingLabel },
    { value: "Descending", label: descendingLabel },
  ] as const;

  function changeSortBy(value: string) {
    const nextSortBy = value as LibrarySortBy;
    const nextSortOrder: LibrarySortOrder = nextSortBy === "Name" ? "Ascending" : "Descending";
    const preference = { sortBy: nextSortBy, sortOrder: nextSortOrder };
    setSortState({ libraryId, preference });
    saveLibrarySortPreference(libraryId, preference);
  }

  function changeSortOrder(value: string) {
    const nextSortOrder = value as LibrarySortOrder;
    const preference = { sortBy, sortOrder: nextSortOrder };
    setSortState({ libraryId, preference });
    saveLibrarySortPreference(libraryId, preference);
  }

  return (
    <section className="lux-page lux-page-narrow">
      <div className="lux-page-heading"><h1>{library?.name || "媒体库"}</h1><p>{total} 项内容</p></div>
      <div className="lux-library-sort-toolbar" aria-label="媒体库排序">
        <div className="lux-library-sort-control"><span>排序</span><LuxSelect value={sortBy} options={sortOptions} onChange={changeSortBy} aria-label="排序方式" /></div>
        <div className="lux-library-sort-control"><span>顺序</span><LuxSelect value={sortOrder} options={orderOptions} onChange={changeSortOrder} aria-label="排序顺序" /></div>
      </div>
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
