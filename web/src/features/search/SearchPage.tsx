import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { Search, UserRound } from "lucide-react";
import { FormEvent, useEffect, useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { api } from "../../lib/api/client";
import type { MediaActor } from "../../lib/api/types";
import { MediaCard } from "../home/media";
import { LibraryGrid } from "../library/LibraryGrid";

export function SearchPage() {
  const [params, setParams] = useSearchParams();
  const initial = params.get("q") ?? "";
  const [query, setQuery] = useState(initial);
  useEffect(() => setQuery(initial), [initial]);
  const result = useInfiniteQuery({
    queryKey: ["search", initial],
    queryFn: ({ pageParam }) => api.search(initial, pageParam),
    initialPageParam: 1,
    maxPages: 5,
    enabled: initial.trim().length > 0,
    getNextPageParam: (lastPage) => {
      const page = lastPage.page ?? 1;
      const pageSize = lastPage.pageSize ?? 24;
      const total = lastPage.total ?? 0;
      return page * pageSize < total ? page + 1 : undefined;
    },
  });
  const mediaItems = result.data?.pages.flatMap((page) => page.items ?? []) ?? [];
  const people = useQuery({
    queryKey: ["people-search", initial],
    queryFn: () => api.searchPeople(initial),
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
        <input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="搜索电影、剧集、演员或别名" aria-label="搜索电影、剧集或演员" />
        <button className="lux-button lux-button-primary" type="submit">搜索</button>
      </form>
      {result.isPending || people.isPending ? <p className="lux-muted-copy">正在搜索…</p> : null}
      {result.error ? <p className="lux-error-copy">媒体搜索失败：{result.error.message}</p> : null}
      {people.error ? <p className="lux-error-copy">人物搜索失败：{people.error.message}</p> : null}
      {people.data?.items?.length ? (
        <section className="lux-search-results-section" aria-labelledby="people-search-heading">
          <div className="lux-section-heading"><h2 id="people-search-heading">人物</h2><span>{people.data.total ?? people.data.items.length} 位</span></div>
          <div className="lux-person-search-grid">
            {people.data.items.map((person) => <PersonSearchCard key={`${person.provider ?? "local"}-${person.id}`} person={person} />)}
          </div>
        </section>
      ) : null}
      {mediaItems.length ? (
        <section className="lux-search-results-section" aria-labelledby="media-search-heading">
          <div className="lux-section-heading"><h2 id="media-search-heading">媒体</h2><span>{result.data?.pages[0]?.total ?? mediaItems.length} 项</span></div>
          <LibraryGrid items={mediaItems} renderItem={(item) => <MediaCard item={item} key={item.id} />} />
          {result.hasNextPage ? (
            <div className="lux-load-more-row">
              <button className="lux-button lux-button-secondary" type="button" disabled={result.isFetchingNextPage} onClick={() => void result.fetchNextPage()}>
                {result.isFetchingNextPage ? "正在加载…" : "加载更多"}
              </button>
            </div>
          ) : null}
        </section>
      ) : null}
      {!result.isPending && !people.isPending && initial && !mediaItems.length && !people.data?.items?.length && !result.error && !people.error ? <div className="lux-empty-card"><span>没有找到匹配内容。</span></div> : null}
      {!initial ? <p className="lux-muted-copy">输入关键词开始搜索。</p> : null}
    </section>
  );
}

function PersonSearchCard({ person }: { person: MediaActor }) {
  return (
    <Link className="lux-person-search-card" to={`/people/${encodeURIComponent(person.id)}`} aria-label={`查看${person.name}的人物详情`}>
      <span className="lux-person-search-avatar">
        {person.imageUrl ? <img src={person.imageUrl} alt="" loading="lazy" decoding="async" /> : <UserRound size={25} aria-hidden="true" />}
      </span>
      <span className="lux-person-search-copy">
        <strong>{person.name}</strong>
        <span>{person.provider ? `${person.provider} · 演员` : "演员"}</span>
      </span>
    </Link>
  );
}
