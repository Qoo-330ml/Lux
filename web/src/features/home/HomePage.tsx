import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Info, Play } from "lucide-react";
import { Link } from "react-router-dom";
import { memo, useEffect, useMemo, useState } from "react";
import { HorizontalScrollRail } from "../../components/layout/HorizontalScrollRail";
import { api } from "../../lib/api/client";
import { queryKeys, queryRefreshIntervals } from "../../lib/api/query-keys";
import { GLOBAL_COLLECTION_LIBRARY_ID } from "../../lib/api/types";
import type { Library, LuxUser, MediaItem } from "../../lib/api/types";
import { readAccountSettings } from "../account/account-settings";
import {
  HERO_CAROUSEL_INTERVAL_MS,
  heroSlides,
  heroTitleScale,
  readHomeCarouselCache,
  writeHomeCarouselCache,
} from "./carousel";
import { useImageVariants } from "./image-variants";
import { ContinueWatchingRail, imageUrl, LibraryCard, MediaRail, mediaTitle, mediaTypeLabel, playbackPositionTicks, runtimeLabel } from "./media";
import { useSseConnectionState, type SseConnectionState } from "../activity/useUserEvents";
import { prefetchLibraryPage } from "../library/prefetchLibrary";

const EMPTY_LIBRARIES: Library[] = [];
const EMPTY_MEDIA_ITEMS: MediaItem[] = [];

export function HomePage({ user }: { user: LuxUser }) {
  const queryClient = useQueryClient();
  const sseState = useSseConnectionState();
  const accountSettings = useMemo(() => readAccountSettings(user.id), [user.id]);
  const cachedCarousel = useMemo(() => readHomeCarouselCache(user.id), [user.id]);
  const carousel = useQuery({
    queryKey: queryKeys.homeCarousel,
    queryFn: ({ signal }) => api.homeCarousel(signal),
    initialData: cachedCarousel?.data,
    initialDataUpdatedAt: cachedCarousel?.savedAt,
    staleTime: 30_000,
    retry: false,
    refetchInterval: (query) => homeRefetchInterval(query.state.data, sseState),
    refetchIntervalInBackground: false,
  });
  const librariesQuery = useQuery({
    queryKey: queryKeys.libraries,
    queryFn: ({ signal }) => api.homeLibraries(signal),
    staleTime: 30_000,
    retry: false,
    refetchInterval: (query) => homeRefetchInterval(query.state.data, sseState),
    refetchIntervalInBackground: false,
  });
  const continueWatchingQuery = useQuery({
    queryKey: queryKeys.homeContinueWatching,
    queryFn: ({ signal }) => api.homeContinueWatching(signal),
    staleTime: 30_000,
    retry: false,
    refetchInterval: (query) => homeRefetchInterval(query.state.data, sseState),
    refetchIntervalInBackground: false,
  });
  const libraries = librariesQuery.data?.libraries ?? EMPTY_LIBRARIES;
  const libraryIds = useMemo(() => libraries.map((library) => library.id), [libraries]);
  const latestLibrariesQuery = useQuery({
    queryKey: queryKeys.homeLatestLibraries(libraryIds),
    queryFn: ({ signal }) => api.homeLibrariesLatest(libraryIds, signal),
    enabled: libraryIds.length > 0,
    staleTime: 30_000,
    retry: false,
    refetchInterval: (query) => homeRefetchInterval(query.state.data, sseState),
    refetchIntervalInBackground: false,
  });
  const latestItemsByLibraryId = useMemo(() => {
    const itemsByLibraryId = new Map<string, MediaItem[]>();
    for (const latest of latestLibrariesQuery.data?.libraries ?? []) {
      itemsByLibraryId.set(latest.libraryId, latest.items);
    }
    return itemsByLibraryId;
  }, [latestLibrariesQuery.data]);

  useEffect(() => {
    if (carousel.data) writeHomeCarouselCache(user.id, carousel.data);
  }, [carousel.data, user.id]);

  const continueWatching = continueWatchingQuery.data?.items ?? EMPTY_MEDIA_ITEMS;
  const slides = useMemo(() => heroSlides({
    recommended: carousel.data?.recommended ?? EMPTY_MEDIA_ITEMS,
    continueWatching,
  }), [carousel.data?.recommended, continueWatching]);
  const heroIsLoading = slides.length === 0 && (
    carousel.isPending || carousel.isFetching ||
    librariesQuery.isPending || librariesQuery.isFetching ||
    continueWatchingQuery.isPending || continueWatchingQuery.isFetching
  );
  const showEmptyHero = slides.length === 0 && !heroIsLoading &&
    carousel.isSuccess && librariesQuery.isSuccess && continueWatchingQuery.isSuccess;
  return (
    <div className="lux-home">
      <HeroCarousel items={slides} continueWatching={continueWatching} isLoading={heroIsLoading} showEmptyState={showEmptyHero} />
      {carousel.error && !carousel.data ? (
        <div className="lux-editor-error" role="status">
          精选轮播加载失败。<button className="lux-button lux-button-secondary" type="button" onClick={() => void carousel.refetch()}>重试</button>
        </div>
      ) : null}
      <div className="lux-home-content">
        {accountSettings.showMediaLibraries ? (
          <section className="lux-section lux-library-section" aria-label="我的媒体库">
            <div className="lux-section-heading"><h2>我的媒体库</h2><span>{librariesQuery.data ? `${libraries.length} 个库` : ""}</span></div>
            <HorizontalScrollRail className="lux-home-rail" ariaLabel="我的媒体库">
              <div className="lux-library-rail">
                {librariesQuery.isPending && !librariesQuery.data ? <div className="lux-skeleton-row" /> : null}
                {librariesQuery.error && !librariesQuery.data ? (
                  <div className="lux-editor-error" role="alert">
                    媒体库加载失败。<button className="lux-button lux-button-secondary" type="button" onClick={() => void librariesQuery.refetch()}>重试</button>
                  </div>
                ) : null}
                {librariesQuery.data && libraries.length ? libraries.map((library) => (
                  <LibraryCard
                    key={library.id}
                    library={library}
                    coverItems={library.id === GLOBAL_COLLECTION_LIBRARY_ID ? latestItemsByLibraryId.get(library.id) : undefined}
                    onPrefetch={() => void prefetchLibraryPage(queryClient, library)}
                  />
                )) : null}
                {librariesQuery.data && !libraries.length ? <EmptyLibraries /> : null}
              </div>
            </HorizontalScrollRail>
          </section>
        ) : null}
        {accountSettings.showContinueWatching && continueWatchingQuery.isPending && !continueWatchingQuery.data ? (
          <section className="lux-section" aria-label="继续观看">
            <div className="lux-section-heading"><h2>继续观看</h2></div>
            <div className="lux-skeleton-row" />
          </section>
        ) : null}
        {accountSettings.showContinueWatching && continueWatching.length ? (
          <ContinueWatchingRail items={continueWatching} total={continueWatchingQuery.data?.total} />
        ) : null}
        {accountSettings.showContinueWatching && continueWatchingQuery.error && !continueWatchingQuery.data ? (
          <div className="lux-editor-error" role="alert">
            继续观看加载失败。<button className="lux-button lux-button-secondary" type="button" onClick={() => void continueWatchingQuery.refetch()}>重试</button>
          </div>
        ) : null}
        {libraries.map((library) => (
          <HomeLatestRail
            key={library.id}
            library={library}
            items={latestItemsByLibraryId.get(library.id)}
            isPending={latestLibrariesQuery.isPending && !latestLibrariesQuery.data}
            hasError={latestLibrariesQuery.error !== null && !latestLibrariesQuery.data}
            onRetry={() => void latestLibrariesQuery.refetch()}
          />
        ))}
      </div>
    </div>
  );
}

export function homeRefetchInterval(data: unknown, sseState: SseConnectionState = "disconnected"): number | false {
  return data === undefined || sseState === "connected" ? false : queryRefreshIntervals.mediaSurface;
}

function HomeLatestRail({
  library,
  items,
  isPending,
  hasError,
  onRetry,
}: {
  library: Library;
  items?: MediaItem[];
  isPending: boolean;
  hasError: boolean;
  onRetry: () => void;
}) {
  const title = `最新${library.name}`;

  if (isPending) {
    return (
      <section className="lux-section" aria-label={title}>
        <div className="lux-section-heading"><h2>{title}</h2></div>
        <div className="lux-skeleton-row" />
      </section>
    );
  }
  if (hasError) {
    return (
      <section className="lux-section" aria-label={title}>
        <div className="lux-section-heading"><h2>{title}</h2></div>
        <div className="lux-editor-error" role="alert">
          最新资源加载失败。<button className="lux-button lux-button-secondary" type="button" onClick={onRetry}>重试</button>
        </div>
      </section>
    );
  }
  return <MediaRail title={title} items={items ?? []} linkTo={`/libraries/${library.id}`} />;
}

const HeroCarousel = memo(function HeroCarousel({ items, continueWatching, isLoading, showEmptyState }: {
  items: MediaItem[];
  continueWatching: MediaItem[];
  isLoading: boolean;
  showEmptyState: boolean;
}) {
  const [activeIndex, setActiveIndex] = useState(0);
  const { enabled } = useImageVariants();
  const slideKey = useMemo(() => items.map((item) => item.id).join("|"), [items]);

  useEffect(() => setActiveIndex(0), [slideKey]);

  useEffect(() => {
    if (items.length < 2) return undefined;
    const timeout = window.setTimeout(
      () => setActiveIndex((index) => (index + 1) % items.length),
      HERO_CAROUSEL_INTERVAL_MS,
    );
    return () => window.clearTimeout(timeout);
  }, [activeIndex, items.length, slideKey]);

  const safeIndex = items.length ? activeIndex % items.length : 0;
  const item = items[safeIndex];
  const logo = item ? imageUrl(item, "logo") : undefined;
  const image = item
    ? imageUrl(item, "fanart", enabled ? 1280 : undefined)
      ?? imageUrl(item, "poster", enabled ? 1280 : undefined)
    : undefined;
  const title = item ? mediaTitle(item) : "你的私人影院";
  const titleClassName = logo
    ? "lux-hero-title has-logo"
    : `lux-hero-title lux-hero-title--${heroTitleScale(title)}`;
  const playbackItem = item ? heroPlaybackItem(item, continueWatching) : undefined;
  const playbackHref = playbackItem
    ? `/watch/${playbackItem.id}`
    : item
      ? `/items/${item.id}`
      : "/libraries";
  const playbackLabel = playbackItem && playbackPositionTicks(playbackItem) > 0 ? "继续播放" : "播放";
  const goTo = (index: number) => setActiveIndex((index + items.length) % items.length);

  return (
    <section className="lux-hero" aria-label="精选媒体轮播" aria-roledescription="carousel" aria-busy={isLoading}>
      {image ? (
        <img
          key={`backdrop-${item?.id}`}
          className="lux-hero-backdrop lux-hero-backdrop-animated"
          src={image}
          alt=""
          decoding="async"
          fetchPriority="high"
        />
      ) : <div className="lux-hero-backdrop lux-hero-backdrop-empty" />}
      <div className="lux-hero-overlay" />
      {item || showEmptyState ? (
          <div key={item?.id ?? "empty"} className="lux-hero-copy lux-hero-copy-animated" role="group" aria-roledescription="slide" aria-label={item ? `第 ${safeIndex + 1} 条精选，共 ${items.length} 条：${mediaTitle(item)}` : "Lux 精选内容"}>
            <h1 className={titleClassName}>
              {logo ? <img className="lux-hero-logo" src={logo} alt={item ? mediaTitle(item) : "Lux 精选内容"} decoding="async" /> : <span className="lux-hero-title-text">{title}</span>}
            </h1>
            <div className="lux-hero-meta">
              {item?.productionYear ? <span>{item.productionYear}</span> : null}
              {item?.itemType ? <span>{mediaTypeLabel(item.itemType)}</span> : null}
              {runtimeLabel(item?.runtimeTicks) ? <span>{runtimeLabel(item?.runtimeTicks)}</span> : null}
            </div>
            <p>{item?.overview || "在属于你的空间里，继续观看收藏的电影与剧集。"}</p>
            <div className="lux-hero-action-row">
              <div className="lux-hero-actions">
                <Link className="lux-button lux-button-large lux-button-primary" to={playbackHref}><Play size={17} fill="currentColor" /> {item ? playbackLabel : "浏览媒体库"}</Link>
                {item ? <Link className="lux-button lux-button-large lux-button-glass" to={`/items/${item.id}`}><Info size={17} /> 详情</Link> : null}
              </div>
              {items.length > 1 ? <div className="lux-hero-carousel-controls" aria-label="选择精选媒体"><div className="lux-hero-dots">{items.map((slide, index) => <button key={slide.id} className={index === safeIndex ? "lux-hero-dot is-active" : "lux-hero-dot"} type="button" aria-label={`显示第 ${index + 1} 条精选：${mediaTitle(slide)}`} aria-current={index === safeIndex ? "true" : undefined} onClick={() => goTo(index)}>{index === safeIndex ? <span className="lux-hero-dot-progress" aria-hidden="true" style={{ animationDuration: `${HERO_CAROUSEL_INTERVAL_MS}ms` }} /> : null}</button>)}</div></div> : null}
            </div>
          </div>
      ) : null}
    </section>
  );
});

function heroPlaybackItem(item: MediaItem, continueWatching: MediaItem[]) {
  if (item.itemType === "SERIES") {
    return continueWatching.find((candidate) => candidate.itemType === "EPISODE" && candidate.seriesId === item.id);
  }
  return item.itemType === "MOVIE" || item.itemType === "EPISODE" ? item : undefined;
}

function EmptyLibraries() {
  return <div className="lux-empty-card"><span>还没有可访问的媒体库</span><Link to="/libraries">查看设置</Link></div>;
}
