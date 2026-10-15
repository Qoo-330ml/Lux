import { memo, useEffect, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import { ArrowLeft, Sparkles, Star, X } from "lucide-react";
import { api } from "../../lib/api/client";
import { queryKeys, queryRefreshIntervals } from "../../lib/api/query-keys";
import type { MediaItem } from "../../lib/api/types";
import { episodeTitle, imageUrl, mediaTitle, posterUrlWithFallback, runtimeLabel } from "../home/media";
import { useMediaSource } from "./MediaSourcePanel";
import { sourceBadges } from "./media-source";

export function detailAddedAt(media: MediaItem) {
  const addedAtCandidate = typeof media.addedAt === "number" && Number.isFinite(media.addedAt)
    ? new Date(media.addedAt * 1000)
    : undefined;
  const addedAt = addedAtCandidate && Number.isFinite(addedAtCandidate.getTime())
    ? addedAtCandidate
    : undefined;
  const addedAtLabel = addedAt?.toLocaleString("zh-CN", {
    year: "numeric",
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
  });
  return { addedAt, addedAtLabel };
}
export function useSeriesContext(media: MediaItem) {
  const seriesId = media.itemType !== "SERIES" ? media.seriesId ?? media.parentId ?? undefined : undefined;
  return useQuery({
    queryKey: queryKeys.item(seriesId ?? ""), queryFn: () => api.item(seriesId ?? ""),
    enabled: Boolean(seriesId), refetchInterval: queryRefreshIntervals.mediaSurface,
  }).data;
}

function useDetailIdentity(media: MediaItem) {
  const series = useSeriesContext(media);
  const seriesImageFallback = media.itemType === "SEASON" ? series : undefined;
  const backdrop = imageUrl(media, "fanart") ?? imageUrl(media)
    ?? (seriesImageFallback ? imageUrl(seriesImageFallback, "fanart") ?? imageUrl(seriesImageFallback) : undefined);
  const poster = posterUrlWithFallback(media, seriesImageFallback);
  return { series, backdrop, poster };
}

export const MediaDetailArtwork = memo(function MediaDetailArtwork({ media, placement }: { media: MediaItem; placement: "backdrop" | "poster" }) {
  const { backdrop, poster } = useDetailIdentity(media);
  const isEpisode = media.itemType === "EPISODE";
  if (placement === "backdrop") return backdrop ? <img className="lux-detail-backdrop" src={backdrop} alt="" /> : null;
  return (
          <div className="lux-detail-poster-column">
            <div className={`lux-detail-poster${isEpisode ? " is-landscape" : ""}${isEpisode && poster ? " has-portrait" : ""}`}>
              {isEpisode && backdrop
                ? <>
                  <img className="lux-detail-poster-landscape" src={backdrop} alt={`${mediaTitle(media)} 剧照`} />
                  {poster ? <img className="lux-detail-poster-portrait" src={poster} alt={`${mediaTitle(media)} 海报`} /> : null}
                </>
                : poster
                  ? <img src={poster} alt={`${mediaTitle(media)} 海报`} />
                  : <span><Sparkles size={32} />{mediaTitle(media)}</span>}
            </div>
          </div>
  );
});

const selectLogo = (response: Awaited<ReturnType<typeof api.itemImages>>) =>
  response.images?.find((image) => image.imageType.toUpperCase() === "LOGO")?.url ?? null;

export const MediaDetailHero = memo(function MediaDetailHero({ media, collectionId }: { media: MediaItem; collectionId?: string }) {
  const series = useSeriesContext(media);
  const { source, selectedAudioStreamIndex } = useMediaSource();
  const logo = useQuery({
    queryKey: queryKeys.itemImages(media.id), queryFn: () => api.itemImages(media.id),
    refetchInterval: queryRefreshIntervals.mediaSurface, select: selectLogo,
  });
  const logoUrl = logo.data ?? imageUrl(media, "logo");
  const isSeries = media.itemType === "SERIES";
  const isSeason = media.itemType === "SEASON";
  const isEpisode = media.itemType === "EPISODE";
  const isVideo = media.itemType === "VIDEO";
  const detailTitle = isSeason || isEpisode ? mediaTitle(series ?? media) : mediaTitle(media);
  const detailOriginalTitle = media.originalTitle?.trim();
  const { addedAt, addedAtLabel } = detailAddedAt(media);
  const premiereDate = media.premiereDate ?? media.nfo?.premiered ?? media.nfo?.releaseDate;
  const releaseYear = media.productionYear ?? (premiereDate ? String(premiereDate).match(/^\d{4}/)?.[0] : undefined);
  const rating = media.rating ?? media.nfo?.rating;
  const runtimeTicks = media.runtimeTicks ?? runtimeTicksFromMinutes(media.nfo?.runtime);
  const badges = sourceBadges(source, selectedAudioStreamIndex);
  const detailSubtitle = isSeason ? `第 ${media.parentIndexNumber ?? ""} 季` : isEpisode ? episodeTitle(media) : undefined;
  return <>
            {collectionId ? <Link className="lux-detail-collection-return" to={`/collections/${encodeURIComponent(collectionId)}`}><ArrowLeft size={15} aria-hidden="true" /> 返回合集</Link> : null}
            <div className="lux-detail-title-row" data-has-logo={logoUrl ? "true" : undefined}>
              {logoUrl ? <img className="lux-detail-logo" src={logoUrl} alt={`${mediaTitle(media)} 徽标`} /> : null}
              <div className="lux-detail-title-names">
                <h1>{detailTitle}</h1>
                {detailOriginalTitle && detailOriginalTitle !== detailTitle
                  ? <span className="lux-detail-original-title">{detailOriginalTitle}</span>
                  : null}
              </div>
              {detailSubtitle ? <p className="lux-detail-subtitle">{detailSubtitle}</p> : null}
            </div>
            <div className="lux-detail-meta">
              {isVideo ? <span>其他视频</span> : null}
              {isSeries ? (
                premiereDate ? (
                  <span>首播 {premiereDate}</span>
                ) : releaseYear ? (
                  <span>{releaseYear}</span>
                ) : null
              ) : releaseYear ? (
                <span>{releaseYear}</span>
              ) : null}
              {isVideo && !releaseYear && addedAt && addedAtLabel ? (
                <span>添加于 <time dateTime={addedAt.toISOString()}>{addedAtLabel}</time></span>
              ) : null}
              {isSeries && media.seasonCount != null ? <span>{media.seasonCount} 季</span> : null}
              {isSeries && media.episodeCount != null ? <span>{media.episodeCount} 集</span> : null}
              {rating != null && Number.isFinite(rating) ? (
                <span className="lux-detail-meta-rating">
                  <Star size={13} className="lux-rating-star" fill="currentColor" aria-hidden="true" />
                  <span>{rating.toFixed(1)} 评分</span>
                </span>
              ) : null}
              {runtimeLabel(runtimeTicks) ? <span>{runtimeLabel(runtimeTicks)}</span> : null}
              {badges.length ? (
                <span className="lux-detail-meta-badges" aria-label="音画规格">
                  {badges.map((badge) => (
                    <span
                      key={badge.key}
                      className={`lux-badge-pill lux-detail-badge ${badge.className ?? ""}`}
                      title={badge.title}
                    >
                      {badge.label}
                    </span>
                  ))}
                </span>
              ) : null}
            </div>
            <ExpandableOverview overview={media.overview || "暂无简介。"} />
  </>;
});

function runtimeTicksFromMinutes(minutes?: number | null) {
  if (minutes == null || !Number.isFinite(minutes) || minutes < 0) return undefined;
  return minutes * 60 * 10_000_000;
}

function ExpandableOverview({ overview }: { overview: string }) {
  const [isOpen, setIsOpen] = useState(false);
  const [isOverflowing, setIsOverflowing] = useState(overview.length > 120);
  const overviewTextRef = useRef<HTMLParagraphElement>(null);
  const moreButtonRef = useRef<HTMLButtonElement>(null);
  const closeButtonRef = useRef<HTMLButtonElement>(null);
  const wasOpenRef = useRef(false);

  useEffect(() => {
    const element = overviewTextRef.current;
    if (!element) return undefined;

    const measureOverflow = () => {
      if (element.scrollHeight === 0 && element.clientHeight === 0) return;
      setIsOverflowing(element.scrollHeight > element.clientHeight + 1);
    };
    measureOverflow();
    if (typeof ResizeObserver === "undefined") return undefined;
    const observer = new ResizeObserver(measureOverflow);
    observer.observe(element);
    return () => observer.disconnect();
  }, [overview]);

  useEffect(() => {
    if (!isOpen) {
      if (wasOpenRef.current) moreButtonRef.current?.focus();
      wasOpenRef.current = false;
      return undefined;
    }

    wasOpenRef.current = true;
    closeButtonRef.current?.focus();
    const previousOverflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setIsOpen(false);
    };
    document.addEventListener("keydown", closeOnEscape);

    return () => {
      document.removeEventListener("keydown", closeOnEscape);
      document.body.style.overflow = previousOverflow;
    };
  }, [isOpen]);

  return (
    <>
      <div className="lux-detail-overview">
        <p ref={overviewTextRef} className="lux-detail-overview-text">{overview}</p>
        {isOverflowing ? (
          <span className="lux-detail-overview-more-wrap">
            <span aria-hidden="true">...</span>
            <button
              ref={moreButtonRef}
              className="lux-detail-overview-more is-underlined"
              type="button"
              onClick={() => setIsOpen(true)}
              aria-haspopup="dialog"
            >
              更多
            </button>
          </span>
        ) : null}
      </div>
      {isOpen ? (
        <div
          className="lux-detail-overview-dialog-backdrop"
          onMouseDown={(event) => {
            if (event.target === event.currentTarget) setIsOpen(false);
          }}
        >
          <section className="lux-detail-overview-dialog" role="dialog" aria-modal="true" aria-labelledby="detail-overview-heading">
            <header className="lux-detail-overview-dialog-header">
              <div>
                <h2 id="detail-overview-heading">详细简介</h2>
              </div>
              <button ref={closeButtonRef} className="lux-detail-overview-dialog-close" type="button" aria-label="关闭详细信息" onClick={() => setIsOpen(false)}>
                <X size={19} />
              </button>
            </header>
            <div className="lux-detail-overview-dialog-body">
              <p>{overview}</p>
            </div>
          </section>
        </div>
      ) : null}
    </>
  );
}
