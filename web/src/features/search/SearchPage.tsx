import { useQuery } from "@tanstack/react-query";
import { Search } from "lucide-react";
import { FormEvent, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { api } from "../../lib/api/client";
import { MediaCard } from "../home/media";

export function SearchPage() {
  const [params, setParams] = useSearchParams();
  const initial = params.get("q") ?? "";
  const [query, setQuery] = useState(initial);
  const result = useQuery({
    queryKey: ["search", initial],
    queryFn: () => api.search(initial),
    enabled: initial.trim().length > 0,
  });

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const value = query.trim();
    if (value) setParams({ q: value });
  }

  return (
    <section className="lux-page lux-page-narrow">
      <div className="lux-page-heading"><h1>搜索</h1></div>
      <form className="lux-search-panel" onSubmit={submit}>
        <Search size={20} aria-hidden="true" />
        <input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="搜索电影、剧集或别名" aria-label="搜索媒体" />
        <button className="lux-button lux-button-primary" type="submit">搜索</button>
      </form>
      {result.isPending ? <p className="lux-muted-copy">正在搜索…</p> : null}
      {result.error ? <p className="lux-error-copy">{result.error.message}</p> : null}
      {result.data?.items?.length ? <div className="lux-poster-grid">{result.data.items.map((item) => <MediaCard item={item} key={item.id} />)}</div> : initial ? <div className="lux-empty-card"><span>没有找到匹配内容。</span></div> : <p className="lux-muted-copy">输入关键词开始搜索。</p>}
    </section>
  );
}
