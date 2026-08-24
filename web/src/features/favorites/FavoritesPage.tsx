import { useState } from "react";
import { Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import { MediaCard } from "../home/media";

export function FavoritesPage() {
  const [page, setPage] = useState(1);
  const favorites = useQuery({
    queryKey: [...queryKeys.favorites, page],
    queryFn: () => api.favorites(page),
  });

  if (favorites.isPending) return <section className="lux-page-state"><p>正在加载收藏…</p></section>;
  if (favorites.error) return <section className="lux-page-state"><h1>收藏加载失败</h1><p>{favorites.error.message}</p></section>;

  const items = favorites.data.items ?? [];
  const total = favorites.data.total ?? 0;
  const pageSize = favorites.data.pageSize ?? 24;
  const currentPage = favorites.data.page ?? page;
  const hasPrevious = currentPage > 1;
  const hasNext = currentPage * pageSize < total;

  return (
    <section className="lux-page lux-page-narrow">
      <div className="lux-page-heading"><h1>我的收藏</h1><p>共 {total} 项</p></div>
      {items.length ? (
        <div className="lux-poster-grid">
          {items.map((item) => <MediaCard item={item} key={item.id} />)}
        </div>
      ) : (
        <div className="lux-empty-card"><span>还没有收藏内容。</span><Link to="/libraries">去媒体库看看</Link></div>
      )}
      {total > 0 ? (
        <div className="lux-admin-pagination" aria-label="收藏分页">
          <span>第 {currentPage} / {Math.max(1, Math.ceil(total / pageSize))} 页</span>
          <div>
            <button className="lux-button lux-button-secondary" type="button" disabled={!hasPrevious} onClick={() => setPage((value) => value - 1)}>上一页</button>
            <button className="lux-button lux-button-secondary" type="button" disabled={!hasNext} onClick={() => setPage((value) => value + 1)}>下一页</button>
          </div>
        </div>
      ) : null}
    </section>
  );
}
