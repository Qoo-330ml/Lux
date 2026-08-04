import { useQuery } from "@tanstack/react-query";
import { AnimatePresence, motion } from "framer-motion";
import { ChevronLeft, ChevronRight, Info, Play } from "lucide-react";
import { Link } from "react-router-dom";
import { useEffect, useState } from "react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { MediaItem } from "../../lib/api/types";
import { HERO_CAROUSEL_INTERVAL_MS, heroSlides } from "./carousel";
import { ContinueWatchingRail, imageUrl, LibraryCard, MediaRail, mediaTitle, mediaTypeLabel, playbackPositionTicks, runtimeLabel } from "./media";

export function HomePage() {
  const home = useQuery({ queryKey: queryKeys.home, queryFn: () => api.home() });

  if (home.isPending) return <HomeSkeleton />;
  if (home.error) return <section className="lux-page-state"><h1>首页加载失败</h1><p>{home.error.message}</p></section>;

  const data = home.data;
  const slides = heroSlides(data);
  return (
    <div className="lux-home">
      <HeroCarousel items={slides} />
      <div className="lux-home-content">
        <section className="lux-section lux-library-section" aria-label="我的媒体库">
          <div className="lux-section-heading"><h2>我的媒体库</h2><span>{data.libraries?.length ?? 0} 个库</span></div>
          <div className="lux-library-rail">
            {data.libraries?.length ? data.libraries.map((library) => <LibraryCard key={library.id} library={library} />) : <EmptyLibraries />}
          </div>
        </section>
        <ContinueWatchingRail items={data.continueWatching ?? []} />
        {data.libraries?.map((library) => <MediaRail key={`latest-${library.id}`} title={`最新${library.name}`} items={library.latest ?? []} />)}
      </div>
    </div>
  );
}

function HeroCarousel({ items }: { items: MediaItem[] }) {
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
  const image = item ? imageUrl(item, "fanart") ?? imageUrl(item) : undefined;
  const goTo = (index: number) => setActiveIndex((index + items.length) % items.length);

  return (
    <section className="lux-hero" aria-label="精选媒体轮播" aria-roledescription="carousel">
      <AnimatePresence initial={false}>
        {image ? <motion.img key={`backdrop-${item?.id}`} className="lux-hero-backdrop" src={image} alt="" initial={{ opacity: 0, scale: 1.04 }} animate={{ opacity: 1, scale: 1.015 }} exit={{ opacity: 0 }} transition={{ duration: 0.55, ease: "easeOut" }} /> : <div className="lux-hero-backdrop lux-hero-backdrop-empty" />}
      </AnimatePresence>
      <div className="lux-hero-overlay" />
      <AnimatePresence initial={false} mode="wait">
        <motion.div key={item?.id ?? "empty"} className="lux-hero-copy" role="group" aria-roledescription="slide" aria-label={item ? `第 ${safeIndex + 1} 条精选，共 ${items.length} 条：${mediaTitle(item)}` : "Lux 精选内容"} initial={{ opacity: 0, y: 18 }} animate={{ opacity: 1, y: 0 }} exit={{ opacity: 0, y: -10 }} transition={{ duration: 0.38 }}>
          <span className="lux-eyebrow">{items.length > 1 ? "LUX DAILY SELECTION" : "LUX ORIGINAL MEDIA SYSTEM"}</span>
          <h1>{item ? mediaTitle(item) : "你的私人影院"}</h1>
          <div className="lux-hero-meta">
            {item?.productionYear ? <span>{item.productionYear}</span> : null}
            {item?.itemType ? <span>{mediaTypeLabel(item.itemType)}</span> : null}
            {runtimeLabel(item?.runtimeTicks) ? <span>{runtimeLabel(item?.runtimeTicks)}</span> : null}
          </div>
          <p>{item?.overview || "在属于你的空间里，继续观看收藏的电影与剧集。"}</p>
          <div className="lux-hero-actions">
            {item ? <Link className="lux-button lux-button-primary" to={`/items/${item.id}`}><Play size={17} fill="currentColor" /> {playbackPositionTicks(item) ? "继续播放" : "播放"}</Link> : <Link className="lux-button lux-button-primary" to="/libraries"><Play size={17} fill="currentColor" /> 浏览媒体库</Link>}
            {item ? <Link className="lux-button lux-button-glass" to={`/items/${item.id}`}><Info size={17} /> 详情</Link> : null}
          </div>
          {items.length > 1 ? <div className="lux-hero-carousel-controls" aria-label="选择精选媒体"><button className="lux-hero-carousel-arrow" type="button" aria-label="上一条精选" onClick={() => goTo(safeIndex - 1)}><ChevronLeft size={17} /></button><div className="lux-hero-dots">{items.map((slide, index) => <button key={slide.id} className={index === safeIndex ? "lux-hero-dot is-active" : "lux-hero-dot"} type="button" aria-label={`显示第 ${index + 1} 条精选：${mediaTitle(slide)}`} aria-current={index === safeIndex ? "true" : undefined} onClick={() => goTo(index)} />)}</div><button className="lux-hero-carousel-arrow" type="button" aria-label="下一条精选" onClick={() => goTo(safeIndex + 1)}><ChevronRight size={17} /></button></div> : null}
        </motion.div>
      </AnimatePresence>
    </section>
  );
}

function EmptyLibraries() {
  return <div className="lux-empty-card"><span>还没有可访问的媒体库</span><Link to="/libraries">查看设置</Link></div>;
}

function HomeSkeleton() {
  return <div className="lux-home lux-skeleton-page"><div className="lux-hero lux-skeleton-block" /><div className="lux-home-content"><div className="lux-skeleton-line" /><div className="lux-skeleton-row" /><div className="lux-skeleton-row" /></div></div>;
}
