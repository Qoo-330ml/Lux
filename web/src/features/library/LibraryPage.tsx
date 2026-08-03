import { useQuery } from "@tanstack/react-query";
import { Link, useParams } from "react-router-dom";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import { MediaCard } from "../home/media";

export function LibraryPage() {
  const { libraryId = "" } = useParams();
  const libraries = useQuery({ queryKey: queryKeys.libraries, queryFn: () => api.libraries() });
  const items = useQuery({ queryKey: queryKeys.library(libraryId, 1), queryFn: () => api.libraryItems(libraryId), enabled: Boolean(libraryId) });
  const library = libraries.data?.libraries?.find((entry) => entry.id === libraryId);

  if (items.isPending || libraries.isPending) return <section className="lux-page-state"><p>正在整理媒体库…</p></section>;
  if (items.error) return <section className="lux-page-state"><h1>媒体库加载失败</h1><p>{items.error.message}</p></section>;

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
