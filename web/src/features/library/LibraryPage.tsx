import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useParams, useSearchParams } from "react-router-dom";
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

function LibraryPageLoadingState({ libraryName }: { libraryName?: string }) {
  return (
    <section className="lux-page lux-page-narrow lux-library-page-loading" aria-busy="true">
      <div className="lux-page-heading">
        <span className="lux-skeleton-line lux-library-page-skeleton-title" aria-hidden="true" />
        {libraryName ? <p>{libraryName} · 正在加载首屏内容…</p> : <p>正在加载媒体库…</p>}
      </div>
      <div className="lux-library-sort-toolbar" aria-hidden="true">
        <span className="lux-skeleton-line lux-library-page-skeleton-control" />
        <span className="lux-skeleton-line lux-library-page-skeleton-control" />
        <span className="lux-skeleton-line lux-library-page-skeleton-control" />
      </div>
      <div className="lux-poster-grid" aria-hidden="true">
        {Array.from({ length: 12 }, (_, index) => <span className="lux-library-page-skeleton-card" key={index} />)}
      </div>
    </section>
  );
}

export function LibraryPage({ serverName }: { serverName?: string | null } = {}) {
  const { libraryId = "" } = useParams();
  const queryClient = useQueryClient();
  const [searchParams, setSearchParams] = useSearchParams();
  const metadataStatus = searchParams.get("metadataStatus")?.toUpperCase() === "PENDING" ? "PENDING" as const : undefined;
  const [sortState, setSortState] = useState(() => ({
    libraryId,
    preference: readLibrarySortPreference(libraryId),
  }));
  const [selectionMode, setSelectionMode] = useState(false);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(() => new Set());
  const [confirmNotice, setConfirmNotice] = useState<string>();
  const sortPreference = sortState.libraryId === libraryId ? sortState.preference : readLibrarySortPreference(libraryId);
  const { sortBy, sortOrder } = sortPreference;
  const libraries = useQuery({ queryKey: queryKeys.libraries, queryFn: () => api.libraries() });
  const library = libraries.data?.libraries?.find((entry) => entry.id === libraryId);
  const showMetadataPending = libraries.data?.showMetadataPending ?? true;
  const itemTypes = libraryItemTypeFilter(library?.kind);
  const confirmMetadata = useMutation({
    mutationFn: () => api.confirmAdminMetadata([...selectedIds]),
    onSuccess: (result) => {
      setConfirmNotice(result.failedCount > 0
        ? `已确认 ${result.confirmedCount} 项，${result.failedCount} 项确认失败，请打开单项处理。`
        : `已确认 ${result.confirmedCount} 项元数据。`);
      if (result.confirmedCount > 0) {
        setSelectedIds(new Set());
        setSelectionMode(false);
      }
      void queryClient.invalidateQueries({ queryKey: ["library", libraryId] });
      void queryClient.invalidateQueries({ queryKey: queryKeys.libraries });
    },
  });

  useEffect(() => {
    setSelectedIds(new Set());
    setSelectionMode(false);
  }, [libraryId, metadataStatus, sortBy, sortOrder]);

  useEffect(() => {
    if (typeof document === "undefined") return;

    const libraryName = library?.name.trim();
    document.title = libraryName ? `${libraryName} - Lux` : "Lux";
    return () => {
      const serverTitle = serverName?.trim();
      document.title = serverTitle ? `${serverTitle} - Lux` : "Lux";
    };
  }, [library?.name, serverName]);

  const pages = useInfiniteQuery({
    queryKey: queryKeys.library(libraryId, 1, itemTypes, sortBy, sortOrder, metadataStatus ?? "all"),
    queryFn: ({ pageParam }) => api.libraryItems(libraryId, pageParam, itemTypes, {
      sortBy,
      sortOrder,
      ...(metadataStatus ? { metadataStatus } : {}),
    }),
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

  if (libraries.isPending) return <LibraryPageLoadingState />;
  if (pages.error && !pages.data) return <section className="lux-page-state"><h1>媒体库加载失败</h1><p>{pages.error.message}</p></section>;
  if (!library) return <section className="lux-page-state"><h1>媒体库不存在</h1><p>这个媒体库可能已被删除或你没有访问权限。</p></section>;
  if (pages.isPending) return <LibraryPageLoadingState libraryName={library.name} />;

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
  const metadataOptions = [
    { value: "ALL", label: "全部内容" },
    { value: "PENDING", label: "待确认" },
  ] as const;
  const selectedItems = loadedItems.filter((item) => selectedIds.has(item.id));
  const allSelectedPending = selectedIds.size > 0
    && selectedItems.length === selectedIds.size
    && selectedItems.every((item) => item.metadataPending === true);

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

  function changeMetadataStatus(value: string) {
    const next = new URLSearchParams(searchParams);
    if (value === "PENDING") next.set("metadataStatus", "pending");
    else next.delete("metadataStatus");
    setSearchParams(next);
  }

  return (
    <section className="lux-page lux-page-narrow">
      <div className="lux-page-heading"><h1>{library?.name || "媒体库"}</h1><p>{metadataStatus ? `${total} 项待确认内容` : `${total} 项内容`}</p></div>
      <div className="lux-library-selection-toolbar" aria-label="媒体库批量操作">
        <button
          className="lux-button lux-button-secondary"
          type="button"
          aria-label={selectionMode ? "退出媒体库多选" : "开启媒体库多选"}
          aria-pressed={selectionMode}
          onClick={() => {
            setSelectionMode((value) => !value);
            setSelectedIds(new Set());
            setConfirmNotice(undefined);
          }}
        >
          {selectionMode ? "退出多选" : "多选"}
        </button>
        {selectionMode && selectedIds.size > 0 ? <span>{`已选 ${selectedIds.size} 项`}</span> : null}
        {allSelectedPending ? <button className="lux-button lux-button-primary" type="button" data-action="batch-confirm-metadata" disabled={confirmMetadata.isPending} onClick={() => confirmMetadata.mutate()}>{confirmMetadata.isPending ? "确认中…" : "批量确认"}</button> : null}
        {confirmNotice ? <span className="lux-muted-copy" role="status">{confirmNotice}</span> : null}
        {confirmMetadata.error ? <span className="lux-error-copy" role="alert">{confirmMetadata.error.message}</span> : null}
      </div>
      <div className="lux-library-sort-toolbar" aria-label="媒体库排序">
        <div className="lux-library-sort-control"><span>元数据</span><LuxSelect value={metadataStatus ?? "ALL"} options={metadataOptions} onChange={changeMetadataStatus} aria-label="元数据状态" /></div>
        <div className="lux-library-sort-control"><span>排序</span><LuxSelect value={sortBy} options={sortOptions} onChange={changeSortBy} aria-label="排序方式" /></div>
        <div className="lux-library-sort-control"><span>顺序</span><LuxSelect value={sortOrder} options={orderOptions} onChange={changeSortOrder} aria-label="排序顺序" /></div>
      </div>
      <div className="lux-poster-grid">
        {loadedItems.map((item) => <MediaCard item={item} key={item.id} metadataAttention={showMetadataPending && Boolean(item.metadataPending)} detailSearch={metadataStatus === "PENDING" ? "?metadataStatus=pending" : undefined} selectionMode={selectionMode} selected={selectedIds.has(item.id)} onSelectionChange={(selected) => setSelectedIds((current) => { const next = new Set(current); if (selected) next.add(item.id); else next.delete(item.id); return next; })} />)}
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
