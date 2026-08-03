import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import { LibraryCard } from "../home/media";

export function LibrariesPage() {
  const libraries = useQuery({ queryKey: queryKeys.libraries, queryFn: () => api.libraries() });

  if (libraries.isPending) return <section className="lux-page-state"><p>正在加载媒体库…</p></section>;
  if (libraries.error) return <section className="lux-page-state"><h1>媒体库加载失败</h1><p>{libraries.error.message}</p></section>;

  const items = libraries.data.libraries ?? [];
  return (
    <section className="lux-page lux-page-narrow">
      <div className="lux-page-heading"><span className="lux-eyebrow">YOUR COLLECTION</span><h1>媒体库</h1><p>浏览你有权限访问的 Lux 内容。</p></div>
      <div className="lux-library-grid">
        {items.length ? items.map((library) => <LibraryCard key={library.id} library={library} />) : <div className="lux-empty-card"><span>当前账号没有可访问的媒体库。</span><Link to="/">返回首页</Link></div>}
      </div>
    </section>
  );
}
