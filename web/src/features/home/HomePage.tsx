import { useQuery } from "@tanstack/react-query";
import { motion } from "framer-motion";
import { Info, Play } from "lucide-react";
import { Link } from "react-router-dom";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { MediaItem } from "../../lib/api/types";
import { imageUrl, LibraryCard, MediaRail, mediaTitle, runtimeLabel } from "./media";

export function HomePage() {
  const home = useQuery({ queryKey: queryKeys.home, queryFn: () => api.home() });

  if (home.isPending) return <HomeSkeleton />;
  if (home.error) return <section className="lux-page-state"><h1>首页加载失败</h1><p>{home.error.message}</p></section>;

  const data = home.data;
  const featured = data.continueWatching?.[0] ?? data.recentlyAdded?.[0];
  return (
    <div className="lux-home">
      <Hero item={featured} />
      <div className="lux-home-content">
        <section className="lux-section lux-library-section" aria-label="我的媒体库">
          <div className="lux-section-heading"><h2>我的媒体库</h2><span>{data.libraries?.length ?? 0} 个库</span></div>
          <div className="lux-library-rail">
            {data.libraries?.length ? data.libraries.map((library) => <LibraryCard key={library.id} library={library} />) : <EmptyLibraries />}
          </div>
        </section>
        <MediaRail title="继续观看" items={data.continueWatching ?? []} landscape />
        <MediaRail title="最近添加" items={data.recentlyAdded ?? []} />
      </div>
    </div>
  );
}

function Hero({ item }: { item?: MediaItem }) {
  const image = item ? imageUrl(item, "fanart") ?? imageUrl(item) : undefined;
  return (
    <section className="lux-hero" aria-label="精选媒体">
      {image ? <img className="lux-hero-backdrop" src={image} alt="" /> : <div className="lux-hero-backdrop lux-hero-backdrop-empty" />}
      <div className="lux-hero-overlay" />
      <motion.div className="lux-hero-copy" initial={{ opacity: 0, y: 18 }} animate={{ opacity: 1, y: 0 }} transition={{ duration: 0.45 }}>
        <span className="lux-eyebrow">LUX ORIGINAL MEDIA SYSTEM</span>
        <h1>{item ? mediaTitle(item) : "你的私人影院"}</h1>
        <div className="lux-hero-meta">
          {item?.productionYear ? <span>{item.productionYear}</span> : null}
          {item?.itemType ? <span>{item.itemType === "SERIES" ? "剧集" : "电影"}</span> : null}
          {runtimeLabel(item?.runtimeTicks) ? <span>{runtimeLabel(item?.runtimeTicks)}</span> : null}
        </div>
        <p>{item?.overview || "在属于你的空间里，继续观看收藏的电影与剧集。"}</p>
        <div className="lux-hero-actions">
          {item ? <Link className="lux-button lux-button-primary" to={`/items/${item.id}`}><Play size={17} fill="currentColor" /> 继续播放</Link> : <Link className="lux-button lux-button-primary" to="/libraries"><Play size={17} fill="currentColor" /> 浏览媒体库</Link>}
          {item ? <Link className="lux-button lux-button-glass" to={`/items/${item.id}`}><Info size={17} /> 详情</Link> : null}
        </div>
      </motion.div>
    </section>
  );
}

function EmptyLibraries() {
  return <div className="lux-empty-card"><span>还没有可访问的媒体库</span><Link to="/libraries">查看设置</Link></div>;
}

function HomeSkeleton() {
  return <div className="lux-home lux-skeleton-page"><div className="lux-hero lux-skeleton-block" /><div className="lux-home-content"><div className="lux-skeleton-line" /><div className="lux-skeleton-row" /><div className="lux-skeleton-row" /></div></div>;
}
