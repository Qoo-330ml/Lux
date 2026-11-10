import "./watch-history.css";
import { useState } from "react";
import { Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { Play } from "lucide-react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { MediaItem } from "../../lib/api/types";
import { LuxButton } from "../../components/ui";
import { MediaPlaceholder, imageUrl, playbackProgress } from "../home/media-display";
import {
  watchHistoryProgressLabel,
  watchHistorySubtitle,
  watchHistoryTimeLabel,
  watchHistoryTitle,
} from "./watch-history-format";

const HISTORY_PAGE_SIZE = 24;

export function WatchHistoryPage() {
  const [page, setPage] = useState(1);
  const history = useQuery({
    queryKey: [...queryKeys.watchHistory, page],
    queryFn: () => api.watchHistory(page),
  });

  if (history.isPending) return <section className="lux-page-state"><p>正在加载观看历史…</p></section>;
  if (history.error) return <section className="lux-page-state"><h1>观看历史加载失败</h1><p>{history.error.message}</p></section>;

  const items = history.data.items ?? [];
  const total = history.data.total ?? 0;
  const pageSize = history.data.pageSize ?? HISTORY_PAGE_SIZE;
  const currentPage = history.data.page ?? page;
  const totalPages = Math.max(1, Math.ceil(total / pageSize));
  const hasPrevious = currentPage > 1;
  const hasNext = history.data.hasNext ?? currentPage * pageSize < total;

  return (
    <section className="lux-page lux-page-narrow">
      <div className="lux-page-heading"><h1>观看历史</h1><p>共 {total} 项</p></div>
      {items.length ? (
        <ol className="lux-history-list">
          {items.map((item) => <WatchHistoryRow item={item} key={`${item.id}-${item.userData?.lastPlayedAt ?? 0}`} />)}
        </ol>
      ) : (
        <div className="lux-empty-card"><span>还没有观看记录。</span><Link to="/libraries">去媒体库看看</Link></div>
      )}
      {total > 0 ? (
        <div className="lux-history-pagination" aria-label="观看历史分页">
          <span>第 {currentPage} / {totalPages} 页</span>
          <div>
            <LuxButton tone="secondary" type="button" disabled={!hasPrevious} onClick={() => setPage((value) => Math.max(1, value - 1))}>上一页</LuxButton>
            <LuxButton tone="secondary" type="button" disabled={!hasNext} onClick={() => setPage((value) => value + 1)}>下一页</LuxButton>
          </div>
        </div>
      ) : null}
    </section>
  );
}

function WatchHistoryRow({ item }: { item: MediaItem }) {
  const image = imageUrl(item, "poster") ?? imageUrl(item, "thumb") ?? imageUrl(item, "fanart");
  const detailHref = `/items/${item.id}`;
  const title = watchHistoryTitle(item);
  const subtitle = watchHistorySubtitle(item);
  const watchedAt = watchHistoryTimeLabel(item.userData?.lastPlayedAt);
  const progressLabel = watchHistoryProgressLabel(item);
  const progress = playbackProgress(item);
  const resumeLabel = item.userData?.isPlayed ? "重新播放" : "继续播放";

  return (
    <li className="lux-history-row">
      <Link className="lux-history-art" to={detailHref} aria-label={`查看 ${title} 详情`}>
        {image ? <img src={image} alt="" loading="lazy" decoding="async" /> : <MediaPlaceholder item={item} />}
      </Link>
      <div className="lux-history-copy">
        <Link className="lux-history-title" to={detailHref}>{title}</Link>
        <div className="lux-history-meta">
          {subtitle ? <span>{subtitle}</span> : null}
          {watchedAt ? <span className="lux-history-time">{watchedAt}</span> : null}
          {progressLabel ? <span className="lux-history-progress-label">{progressLabel}</span> : null}
        </div>
        {progress > 0 ? <span className="lux-progress lux-history-progress" aria-hidden="true"><span style={{ width: `${progress}%` }} /></span> : null}
      </div>
      <Link className="lux-button lux-button-secondary lux-button-compact lux-history-resume" to={`/watch/${item.id}`}>
        <Play size={14} fill="currentColor" aria-hidden="true" />
        {resumeLabel}
      </Link>
    </li>
  );
}
