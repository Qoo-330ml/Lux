import { memo, useEffect, useState, type ReactNode } from "react";
import { Link, useNavigate } from "react-router-dom";
import { Play } from "lucide-react";
import type { MediaItem } from "../../lib/api/types";
import { EpisodeCount, Rating, episodeTitle, imageUrl, mediaTitle, posterUrlWithFallback } from "../home/media";
import { useMediaEpisodes } from "./useMediaEpisodes";

export const MediaEpisodesSection = memo(function MediaEpisodesSection({ item: media, children }: { item: MediaItem; children: ReactNode }) {
  const { seasons, singleSeason, episodes, episodeItems } = useMediaEpisodes(media);
  const isSeries = media.itemType === "SERIES";
  const isSeason = media.itemType === "SEASON";
  const isEpisode = media.itemType === "EPISODE";
  return <>
          {(!isSeries || singleSeason) ? (
            <div className="lux-detail-hierarchy">
              {isSeries && singleSeason ? (
                <SeasonEpisodes
                  episodes={episodeItems}
                  total={episodes.data?.pages[0]?.total}
                  seasonNumber={singleSeason.parentIndexNumber}
                  episodesPending={episodes.isPending}
                  hasNextPage={episodes.hasNextPage}
                  isFetchingNextPage={episodes.isFetchingNextPage}
                  isFetchNextPageError={episodes.isFetchNextPageError}
                  error={episodes.error}
                  onLoadMore={() => void episodes.fetchNextPage()}
                />
              ) : null}
              {isSeason ? <SeasonEpisodes episodes={episodeItems} total={episodes.data?.pages[0]?.total} seasonNumber={media.parentIndexNumber} episodesPending={episodes.isPending} hasNextPage={episodes.hasNextPage} isFetchingNextPage={episodes.isFetchingNextPage} isFetchNextPageError={episodes.isFetchNextPageError} error={episodes.error} onLoadMore={() => void episodes.fetchNextPage()} /> : null}
              {isEpisode ? <EpisodeRail episodes={episodeItems} currentEpisodeId={media.id} seasonNumber={media.parentIndexNumber} episodesPending={episodes.isPending} /> : null}
            </div>
          ) : null}
    {children}
          {isSeries && !singleSeason ? (
            <div className="lux-detail-series-seasons">
              <SeriesChildren
                seasons={seasons.data?.items ?? []}
                series={media}
              />
            </div>
          ) : null}
  </>;
});

export function MediaEpisodePlayButton({ item, onNotice }: { item: MediaItem; onNotice: (notice: string) => void }) {
  const { episodeItems, pending } = useMediaEpisodes(item);
  const navigate = useNavigate();
  const [playRequested, setPlayRequested] = useState(false);
  const nextId = item.itemType === "SERIES" || item.itemType === "SEASON"
    ? selectNextPlayableEpisode(episodeItems)?.id : undefined;
  useEffect(() => {
    if (!playRequested || pending) return;
    if (nextId) navigate(`/watch/${nextId}`);
    else {
      setPlayRequested(false);
      onNotice("没有可播放的单集。");
    }
  }, [navigate, nextId, onNotice, pending, playRequested]);
  if (nextId) return <Link className="lux-button lux-button-large lux-button-primary" to={`/watch/${nextId}`}><Play size={17} fill="currentColor" /> 播放</Link>;
  return <button className="lux-button lux-button-large lux-button-primary" type="button" onClick={() => {
      setPlayRequested(pending);
      onNotice(pending ? "正在查找可播放单集，请稍候。" : "没有可播放的单集。");
    }}><Play size={17} fill="currentColor" /> 播放</button>;
}

function selectNextPlayableEpisode(episodes: MediaItem[]) {
  const playableEpisodes = episodes.filter((episode) =>
    episode.itemType === "EPISODE" && (episode.mediaSources?.length ?? 0) > 0,
  );
  return playableEpisodes.find((episode) => !episode.userData?.isPlayed) ?? playableEpisodes[0];
}

function SeriesChildren({
  seasons,
  series,
}: {
  seasons: MediaItem[];
  series: MediaItem;
}) {
  return (
    <section className="lux-series-children" aria-labelledby="series-children-heading">
      <div className="lux-section-heading">
        <div className="lux-heading-lead">
          <h2 id="series-children-heading">播出季</h2>
          <span className="lux-section-heading-sub">SEASONS</span>
        </div>
        <span>{seasons.length} 个季度</span>
      </div>
      <div className="lux-season-rail" role="list" aria-label="播出季">
        {seasons.map((season) => {
          const poster = posterUrlWithFallback(season, series);
          return (
            <Link
              className="lux-season-card"
              key={season.id}
              role="listitem"
              to={`/items/${season.id}`}
            >
              <span className="lux-season-card-art">
                {poster ? <img src={poster} alt={`${mediaTitle(season)} 海报`} loading="lazy" /> : <span className="lux-season-card-placeholder">{mediaTitle(season)}</span>}
                <Rating value={season.rating} placement="card" />
                <EpisodeCount item={season} />
              </span>
              <strong>{mediaTitle(season)}</strong>
            </Link>
          );
        })}
      </div>
    </section>
  );
}

function SeasonEpisodes({
  episodes,
  total,
  seasonNumber,
  episodesPending,
  hasNextPage,
  isFetchingNextPage,
  isFetchNextPageError,
  error,
  onLoadMore,
}: {
  episodes: MediaItem[];
  total?: number;
  seasonNumber?: number | null;
  episodesPending: boolean;
  hasNextPage: boolean;
  isFetchingNextPage: boolean;
  isFetchNextPageError: boolean;
  error: Error | null;
  onLoadMore: () => void;
}) {
  return (
    <section className="lux-season-episodes" aria-labelledby="season-episodes-heading">
      <div className="lux-section-heading">
        <div className="lux-heading-lead">
          <h2 id="season-episodes-heading">单集</h2>
          <span className="lux-section-heading-sub">EPISODES</span>
        </div>
        <span>{total ?? episodes.length} 集</span>
      </div>
      {episodesPending ? <p className="lux-muted-copy">正在加载单集…</p> : null}
      {error && !isFetchNextPageError ? <p className="lux-error-copy" role="alert">单集加载失败：{error.message}</p> : null}
      {!episodesPending && episodes.length ? (
        <div className="lux-season-episode-list" role="list">
          {episodes.map((episode, index) => <SeasonEpisodeRow episode={episode} seasonNumber={seasonNumber} fallbackNumber={index + 1} key={episode.id} />)}
        </div>
      ) : null}
      {isFetchNextPageError ? <p className="lux-error-copy" role="alert">加载更多单集失败：{error?.message || "请重试"}</p> : null}
      {!episodesPending && !episodes.length && !error ? <p className="lux-muted-copy">这个季度还没有可播放的单集。</p> : null}
      {hasNextPage ? (
        <div className="lux-person-works-actions">
          <button
            className="lux-button lux-button-secondary"
            type="button"
            data-action="load-more-episodes"
            onClick={onLoadMore}
            disabled={isFetchingNextPage}
          >
            {isFetchingNextPage ? "加载中…" : isFetchNextPageError ? "重试加载" : `加载更多单集（${Math.max(0, (total ?? episodes.length) - episodes.length)} 集）`}
          </button>
        </div>
      ) : null}
    </section>
  );
}

function SeasonEpisodeRow({ episode, seasonNumber, fallbackNumber }: { episode: MediaItem; seasonNumber?: number | null; fallbackNumber: number }) {
  const image = imageUrl(episode, "fanart") ?? imageUrl(episode);
  const number = episode.indexNumber ?? fallbackNumber;
  const detailHref = `/items/${episode.id}`;
  const watchHref = `/watch/${episode.id}`;
  return (
    <article className="lux-season-episode-row" role="listitem">
      <Link className="lux-season-episode-thumb lux-season-episode-play" to={watchHref} aria-label={`播放 ${mediaTitle(episode)}`}>
        {image ? <img src={image} alt="" loading="lazy" /> : <span className="lux-season-episode-placeholder">{mediaTitle(episode)}</span>}
        <span className="lux-season-episode-play-icon" aria-hidden="true"><Play size={22} fill="currentColor" /></span>
      </Link>
      <Link className="lux-season-episode-copy" to={detailHref}>
        <strong>{episodeTitle(episode, seasonNumber, number)}</strong>
        {episode.productionYear ? <small>{episode.productionYear}</small> : null}
        {episode.overview ? <p>{episode.overview}</p> : null}
      </Link>
      <Link className="lux-season-episode-arrow" to={detailHref}>查看详情 →</Link>
    </article>
  );
}

function EpisodeRail({
  episodes,
  currentEpisodeId,
  seasonNumber,
  episodesPending,
}: {
  episodes: MediaItem[];
  currentEpisodeId: string;
  seasonNumber?: number | null;
  episodesPending: boolean;
}) {
  const otherEpisodes = episodes.filter((episode) => episode.id !== currentEpisodeId);
  if (episodesPending || !otherEpisodes.length) return null;
  return (
    <section className="lux-episode-rail" aria-labelledby="episode-rail-heading">
      <div className="lux-section-heading">
        <div className="lux-heading-lead">
          <h2 id="episode-rail-heading">更多来自第 {seasonNumber ?? ""} 季</h2>
          <span className="lux-section-heading-sub">MORE EPISODES</span>
        </div>
        <span>{otherEpisodes.length} 集</span>
      </div>
      <div className="lux-episode-card-rail" role="list">
        {otherEpisodes.map((episode) => {
          const image = imageUrl(episode, "fanart") ?? imageUrl(episode);
          const detailHref = `/items/${episode.id}`;
          return (
            <article className="lux-episode-card" role="listitem" key={episode.id}>
              <Link className="lux-episode-card-art lux-episode-card-play" to={`/watch/${episode.id}`} aria-label={`播放 ${mediaTitle(episode)}`}>
                {image ? <img src={image} alt="" loading="lazy" /> : <span className="lux-episode-card-placeholder">{mediaTitle(episode)}</span>}
                <span className="lux-episode-card-play-icon" aria-hidden="true"><Play size={22} fill="currentColor" /></span>
              </Link>
              <Link className="lux-episode-card-copy" to={detailHref}><strong>{episodeTitle(episode, seasonNumber)}</strong></Link>
            </article>
          );
        })}
      </div>
    </section>
  );
}
