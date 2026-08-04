import { useQuery } from "@tanstack/react-query";
import { Link, useParams } from "react-router-dom";
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
  const items = useQuery({
    queryKey: queryKeys.library(libraryId, 1, itemTypes),
    queryFn: () => api.libraryItems(libraryId, 1, itemTypes),
    enabled: Boolean(libraryId && library),
  });

  if (libraries.isPending) return <section className="lux-page-state"><p>正在整理媒体库…</p></section>;
  if (items.error) return <section className="lux-page-state"><h1>媒体库加载失败</h1><p>{items.error.message}</p></section>;
  if (!library) return <section className="lux-page-state"><h1>媒体库不存在</h1><p>这个媒体库可能已被删除或你没有访问权限。</p></section>;
  if (items.isPending) return <section className="lux-page-state"><p>正在整理媒体库…</p></section>;

  return (
    <section className="lux-page lux-page-narrow">
      <div className="lux-page-heading"><span className="lux-eyebrow">LIBRARY</span><h1>{library?.name || "媒体库"}</h1><p>{items.data.total ?? items.data.items?.length ?? 0} 项内容</p></div>
      <div className="lux-poster-grid">
        {(items.data.items ?? []).map((item) => <MediaCard item={item} key={item.id} />)}
      </div>
      {!items.data.items?.length ? <div className="lux-empty-card"><span>这个媒体库还没有内容。</span><Link to="/libraries">返回媒体库</Link></div> : null}
    </section>
  );
}
