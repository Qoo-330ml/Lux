import { useQuery, useQueryClient } from "@tanstack/react-query";
import { AnimatePresence, motion } from "framer-motion";
import { ChevronLeft, ChevronRight, Info, Play } from "lucide-react";
import { Link } from "react-router-dom";
import { useEffect, useMemo, useState } from "react";
import { HorizontalScrollRail } from "../../components/layout/HorizontalScrollRail";
import { api } from "../../lib/api/client";
import { queryKeys, queryRefreshIntervals } from "../../lib/api/query-keys";
import type { HomeResponse, LuxUser, MediaItem } from "../../lib/api/types";
import { HERO_CAROUSEL_INTERVAL_MS, heroSlides, heroTitleScale } from "./carousel";
import { ContinueWatchingRail, imageUrl, LibraryCard, MediaRail, mediaTitle, mediaTypeLabel, playbackPositionTicks, runtimeLabel } from "./media";
import { prefetchLibraryPage } from "../library/prefetchLibrary";

const HOME_CACHE_VERSION = 1;
const HOME_CACHE_TTL_MS = 5 * 60_000;

export function HomePage({ user }: { user: LuxUser }) {
  const queryClient = useQueryClient();
  const cachedHome = useMemo(() => readHomeCache(user.id), [user.id]);
  const home = useQuery({
    queryKey: queryKeys.home,
    queryFn: () => api.home(),
    initialData: cachedHome?.data,
    initialDataUpdatedAt: cachedHome?.savedAt,
    staleTime: 0,
    refetchInterval: queryRefreshIntervals.mediaSurface,
    refetchIntervalInBackground: false,
  });

  useEffect(() => {
    if (home.data) {
      queryClient.setQueryData(queryKeys.libraries, {
        libraries: home.data.libraries ?? [],
      });
      writeHomeCache(user.id, home.data);
    }
  }, [home.data, queryClient, user.id]);

  if (home.isPending && !home.data) return <HomeSkeleton />;
  if (home.error && !home.data) return <section className="lux-page-state"><h1>首页加载失败</h1><p>{home.error.message}</p></section>;

  const data = home.data ?? {};
  const libraries = data.libraries ?? [];
  const slides = heroSlides(data);
  return (
    <div className="lux-home">
      <HeroCarousel items={slides} continueWatching={data.continueWatching ?? []} />
      <div className="lux-home-content">
        <section className="lux-section lux-library-section" aria-label="我的媒体库">
          <div className="lux-section-heading"><h2>我的媒体库</h2><span>{libraries.length} 个库</span></div>
          <HorizontalScrollRail className="lux-home-rail" ariaLabel="我的媒体库">
            <div className="lux-library-rail">
              {libraries.length ? libraries.map((library) => <LibraryCard key={library.id} library={library} onPrefetch={() => void prefetchLibraryPage(queryClient, library)} />) : <EmptyLibraries />}
            </div>
          </HorizontalScrollRail>
        </section>
        <ContinueWatchingRail items={data.continueWatching ?? []} total={data.continueWatchingTotal} />
        {libraries.map((library) => <MediaRail key={`latest-${library.id}`} title={`最新${library.name}`} items={library.latest ?? []} linkTo={`/libraries/${library.id}`} />)}
      </div>
    </div>
  );
}

function homeCacheKey(userId: string) {
  return `lux.home.v${HOME_CACHE_VERSION}:${encodeURIComponent(userId)}`;
}

function readHomeCache(userId: string): { data: HomeResponse; savedAt: number } | undefined {
  if (!userId || typeof window === "undefined") return undefined;
  try {
    const raw = window.sessionStorage.getItem(homeCacheKey(userId));
    if (!raw) return undefined;
    const parsed: unknown = JSON.parse(raw);
    if (!isRecord(parsed) || parsed.version !== HOME_CACHE_VERSION || !isRecord(parsed.data)) return undefined;
    if (typeof parsed.savedAt !== "number" || !Number.isFinite(parsed.savedAt)) return undefined;
    if (Date.now() - parsed.savedAt > HOME_CACHE_TTL_MS) return undefined;
    return { data: parsed.data as HomeResponse, savedAt: parsed.savedAt };
  } catch {
    return undefined;
  }
}

function writeHomeCache(userId: string, data: HomeResponse) {
  if (!userId || typeof window === "undefined") return;
  try {
    window.sessionStorage.setItem(homeCacheKey(userId), JSON.stringify({
      version: HOME_CACHE_VERSION,
      savedAt: Date.now(),
      data,
    }));
  } catch {
    // Storage can be unavailable in private browsing or when the quota is exhausted.
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function HeroCarousel({ items, continueWatching }: { items: MediaItem[]; continueWatching: MediaItem[] }) {
  const [activeIndex, setActiveIndex] = useState(0);
  const slideKey = items.map((item) => item.id).join("|");

  useEffect(() => setActiveIndex(0), [slideKey]);

  useEffect(() => {
    if (items.length < 2) return undefined;
    const interval = window.setInterval(
      () => setActiveIndex((index) => (index + 1) % items.length),
      HERO_CAROUSEL_INTERVAL_MS,
    );
    return () => window.clearInterval(interval);
  }, [items.length, slideKey]);

  const safeIndex = items.length ? activeIndex % items.length : 0;
  const item = items[safeIndex];
  const logo = item ? imageUrl(item, "logo") : undefined;
  const image = item ? imageUrl(item, "fanart") ?? imageUrl(item) : undefined;
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
    <section className="lux-hero" aria-label="精选媒体轮播" aria-roledescription="carousel">
      <AnimatePresence initial={false}>
        {image ? <motion.img key={`backdrop-${item?.id}`} className="lux-hero-backdrop" src={image} alt="" decoding="async" fetchPriority="high" initial={{ opacity: 0, scale: 1.04 }} animate={{ opacity: 1, scale: 1.015 }} exit={{ opacity: 0 }} transition={{ duration: 0.55, ease: "easeOut" }} /> : <div className="lux-hero-backdrop lux-hero-backdrop-empty" />}
      </AnimatePresence>
      <div className="lux-hero-overlay" />
      <AnimatePresence initial={false} mode="wait">
        <motion.div key={item?.id ?? "empty"} className="lux-hero-copy" role="group" aria-roledescription="slide" aria-label={item ? `第 ${safeIndex + 1} 条精选，共 ${items.length} 条：${mediaTitle(item)}` : "Lux 精选内容"} initial={{ opacity: 0, y: 18 }} animate={{ opacity: 1, y: 0 }} exit={{ opacity: 0, y: -10 }} transition={{ duration: 0.38 }}>
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
            {items.length > 1 ? <div className="lux-hero-carousel-controls" aria-label="选择精选媒体"><button className="lux-hero-carousel-arrow" type="button" aria-label="上一条精选" onClick={() => goTo(safeIndex - 1)}><ChevronLeft size={17} /></button><div className="lux-hero-dots">{items.map((slide, index) => <button key={slide.id} className={index === safeIndex ? "lux-hero-dot is-active" : "lux-hero-dot"} type="button" aria-label={`显示第 ${index + 1} 条精选：${mediaTitle(slide)}`} aria-current={index === safeIndex ? "true" : undefined} onClick={() => goTo(index)} />)}</div><button className="lux-hero-carousel-arrow" type="button" aria-label="下一条精选" onClick={() => goTo(safeIndex + 1)}><ChevronRight size={17} /></button></div> : null}
          </div>
        </motion.div>
      </AnimatePresence>
    </section>
  );
}

function heroPlaybackItem(item: MediaItem, continueWatching: MediaItem[]) {
  if (item.itemType === "SERIES") {
    return continueWatching.find((candidate) => candidate.itemType === "EPISODE" && candidate.seriesId === item.id);
  }
  return item.itemType === "MOVIE" || item.itemType === "EPISODE" ? item : undefined;
}

function EmptyLibraries() {
  return <div className="lux-empty-card"><span>还没有可访问的媒体库</span><Link to="/libraries">查看设置</Link></div>;
}

function HomeSkeleton() {
  return <div className="lux-home lux-skeleton-page"><div className="lux-hero lux-skeleton-block" /><div className="lux-home-content"><div className="lux-skeleton-line" /><div className="lux-skeleton-row" /><div className="lux-skeleton-row" /></div></div>;
}
