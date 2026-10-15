import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowLeft, Check, Heart, Play, Radio, Sparkles, Star, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router-dom";
import { api } from "../../lib/api/client";
import { queryKeys, queryRefreshIntervals } from "../../lib/api/query-keys";
import type { MediaItem } from "../../lib/api/types";
import { MediaSourcePanel, MediaSourceProvider, useMediaSource } from "./MediaSourcePanel";
import { sourceBadges, streamsOfType } from "./media-source";
import { MediaCastSection, MediaNfoSection, MediaStillsSection } from "./MediaDetailSections";
import { episodeTitle, imageUrl, mediaTitle, posterUrlWithFallback, runtimeLabel } from "../home/media";
import { MediaActionMenu } from "../media/MediaActionMenu";
import { MediaImageEditor } from "../media/MediaImageEditor";
import { MediaIdentifier } from "../media/MediaIdentifier";
import { MediaMetadataEditor } from "../media/MediaMetadataEditor";
import { MediaSubtitleEditor } from "../media/MediaSubtitleEditor";
import { MediaDeleteDialog } from "../media/MediaDeleteDialog";
import { MediaEpisodesSection, MediaEpisodePlayButton } from "./MediaEpisodesSection";
import { LuxLogo } from "../../components/LuxLogo";

export function MediaDetailPage() {
  const { itemId = "" } = useParams();
  const [routeParams] = useSearchParams();
  const item = useQuery({
    queryKey: queryKeys.item(itemId),
    queryFn: () => api.item(itemId),
    enabled: Boolean(itemId),
    refetchInterval: queryRefreshIntervals.mediaSurface,
  });
  if (item.isPending) return <section className="lux-page-state"><p>正在加载媒体详情…</p></section>;
  if (item.error) return <section className="lux-page-state"><h1>媒体详情加载失败</h1><p>{item.error.message}</p></section>;
  return <MediaSourceProvider key={itemId} item={item.data} requestedSourceId={routeParams.get("sourceId") ?? undefined}>
    <MediaDetailContent media={item.data} />
  </MediaSourceProvider>;
}

function MediaDetailContent({ media }: { media: MediaItem }) {
  const itemId = media.id;
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();
  const pendingReview = searchParams.get("metadataStatus")?.toUpperCase() === "PENDING";
  const collectionId = searchParams.get("collectionId") ?? undefined;
  const queryClient = useQueryClient();
  const { source, selectedAudioStreamIndex, selectedSubtitleStreamIndex } = useMediaSource();
  const itemImages = useQuery({
    queryKey: queryKeys.itemImages(itemId),
    queryFn: () => api.itemImages(itemId),
    enabled: Boolean(itemId),
    refetchInterval: queryRefreshIntervals.mediaSurface,
  });
  const playback = useQuery({
    queryKey: queryKeys.playback(itemId),
    queryFn: () => api.playback(itemId),
    enabled: Boolean(itemId),
    refetchInterval: queryRefreshIntervals.mediaSurface,
  });
  const pendingItemsQueryKey = queryKeys.library(media.libraryId ?? "", 1, undefined, "Name", "Ascending", "PENDING", 100);
  const pendingItems = useQuery({
    queryKey: pendingItemsQueryKey,
    queryFn: () => api.libraryItems(media.libraryId ?? "", 1, undefined, { metadataStatus: "PENDING", pageSize: 100 }),
    enabled: pendingReview && Boolean(media.libraryId),
  });
  const [editor, setEditor] = useState<"metadata" | "images" | "subtitles" | "identify">();
  const [actionError, setActionError] = useState<string>();
  const [actionNotice, setActionNotice] = useState<string>();
  const [confirmingMetadata, setConfirmingMetadata] = useState(false);
  const [pendingPlaybackAction, setPendingPlaybackAction] = useState<"favorite" | "played">();
  const [deleteOpen, setDeleteOpen] = useState(false);
  const isSeries = media.itemType === "SERIES";
  const isSeason = media.itemType === "SEASON";
  const isEpisode = media.itemType === "EPISODE";
  const isVideo = media.itemType === "VIDEO";
  const hierarchySeriesId = media && !isSeries
    ? media.seriesId ?? media.parentId ?? undefined
    : undefined;
  const seriesContext = useQuery({
    queryKey: queryKeys.item(hierarchySeriesId ?? ""),
    queryFn: () => api.item(hierarchySeriesId ?? ""),
    enabled: Boolean(hierarchySeriesId),
    refetchInterval: queryRefreshIntervals.mediaSurface,
  });
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
  const logo = itemImages.data?.images?.find((image) => image.imageType.toUpperCase() === "LOGO");
  const logoUrl = logo?.url ?? imageUrl(media, "logo");
  const seriesImageFallback = isSeason ? seriesContext.data : undefined;
  const backdrop = imageUrl(media, "fanart")
    ?? imageUrl(media)
    ?? (seriesImageFallback ? imageUrl(seriesImageFallback, "fanart") ?? imageUrl(seriesImageFallback) : undefined);
  const poster = posterUrlWithFallback(media, seriesImageFallback);
  const detailKind = isVideo ? "video" : isSeries ? "series" : isSeason ? "season" : isEpisode ? "episode" : "movie";
  const showStills = media.itemType === "MOVIE" || media.itemType === "SERIES";
  const detailTitle = isSeries || (!isSeason && !isEpisode)
    ? mediaTitle(media)
    : mediaTitle(seriesContext.data ?? media);
  const detailOriginalTitle = media.originalTitle?.trim();
  const localNfo = media.nfo;
  const premiereDate = media.premiereDate ?? localNfo?.premiered ?? localNfo?.releaseDate;
  const releaseYear = media.productionYear
    ?? (premiereDate ? String(premiereDate).match(/^\d{4}/)?.[0] : undefined);
  const badges = sourceBadges(source, selectedAudioStreamIndex);
  const rating = media.rating ?? localNfo?.rating;
  const runtimeTicks = media.runtimeTicks ?? runtimeTicksFromMinutes(localNfo?.runtime);
  const mediaTagline = localNfo?.tagline?.trim();
  const detailSubtitle = isSeason
    ? `第 ${media.parentIndexNumber ?? ""} 季`
    : isEpisode
      ? episodeTitle(media)
      : undefined;
  const watchQuery = source ? new URLSearchParams({ sourceId: source.id }) : undefined;
  const firstAudioStreamIndex = streamsOfType(source, "AUDIO")[0]?.index;
  if (watchQuery && selectedAudioStreamIndex && selectedAudioStreamIndex !== String(firstAudioStreamIndex ?? "")) {
    watchQuery.set("audioStreamIndex", selectedAudioStreamIndex);
  }
  if (watchQuery && selectedSubtitleStreamIndex) watchQuery.set("subtitleStreamIndex", selectedSubtitleStreamIndex);
  const watchHref = source
    ? `/watch/${media.id}?${watchQuery?.toString()}`
    : undefined;
  const pendingReviewItems = pendingItems.data?.items ?? [];
  const pendingIndex = pendingReviewItems.findIndex((entry) => entry.id === media.id);
  const nextPendingItem = pendingIndex >= 0
    ? pendingReviewItems[pendingIndex + 1]
    : pendingReviewItems[0];

  async function setMetadataLock(locked: boolean) {
    setActionError(undefined);
    setActionNotice(undefined);
    try {
      await api.setItemMetadataLock(media.id, locked);
      await queryClient.invalidateQueries({ queryKey: queryKeys.item(media.id) });
    } catch (cause) {
      setActionError(cause instanceof Error ? cause.message : "元数据锁定状态更新失败，请重试。");
    }
  }

  async function refreshMetadata() {
    setActionError(undefined);
    setActionNotice(undefined);
    try {
      await api.startItemMetadataRefresh(media.id);
      setActionNotice("元数据刷新任务已提交，可在管理任务中查看进度。");
    } catch (cause) {
      setActionError(cause instanceof Error ? cause.message : "元数据刷新任务提交失败，请重试。");
    }
  }

  async function confirmMetadata() {
    setActionError(undefined);
    setActionNotice(undefined);
    setConfirmingMetadata(true);
    try {
      const result = await api.confirmAdminMetadata([media.id]);
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: queryKeys.item(media.id) }),
        queryClient.invalidateQueries({ queryKey: ["library", media.libraryId ?? ""] }),
        queryClient.invalidateQueries({ queryKey: pendingItemsQueryKey }),
      ]);
      if (result.failedCount > 0) {
        setActionError("当前待确认候选确认失败，请重新搜索候选后再试。");
      } else {
        setActionNotice("元数据已确认。");
      }
    } catch (cause) {
      setActionError(cause instanceof Error ? cause.message : "元数据确认失败，请重试。");
    } finally {
      setConfirmingMetadata(false);
    }
  }

  async function scanLibrary() {
    setActionError(undefined);
    setActionNotice(undefined);
    try {
      await api.startItemFolderScan(media.id);
      setActionNotice("所在文件夹扫描任务已提交，可在管理任务中查看进度。");
    } catch (cause) {
      setActionError(cause instanceof Error ? cause.message : "媒体库扫描任务提交失败，请重试。");
    }
  }

  async function updatePlaybackFlag(kind: "favorite" | "played") {
    if (!playback.data || pendingPlaybackAction) return;
    setActionError(undefined);
    setActionNotice(undefined);
    setPendingPlaybackAction(kind);
    try {
      if (kind === "favorite") {
        await api.setFavorite(media.id, !playback.data.isFavorite);
        setActionNotice(playback.data.isFavorite ? "已移出收藏。" : "已加入收藏。");
      } else {
        await api.setPlayed(media.id, !playback.data.isPlayed);
        setActionNotice(playback.data.isPlayed ? "已取消已看标记。" : "已标记为已看。");
      }
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: queryKeys.playback(media.id) }),
        queryClient.invalidateQueries({ queryKey: queryKeys.item(media.id) }),
        queryClient.invalidateQueries({ queryKey: queryKeys.home }),
        queryClient.invalidateQueries({ queryKey: queryKeys.favorites }),
      ]);
    } catch (cause) {
      setActionError(cause instanceof Error ? cause.message : "播放状态更新失败，请重试。");
    } finally {
      setPendingPlaybackAction(undefined);
    }
  }

  return (
    <article className={`lux-detail-page lux-detail-page-${detailKind}`}>
      <div className="lux-detail-mobile-header">
        <button className="lux-detail-mobile-back" type="button" aria-label="返回上一页" onClick={() => navigate(-1)}>
          <ArrowLeft size={21} />
        </button>
        <Link className="lux-detail-mobile-brand" to="/" aria-label="Lux 首页">
          <LuxLogo className="lux-detail-mobile-brand-logo" />
          <span>Lux</span>
        </Link>
        <MediaActionMenu item={media} className="lux-detail-mobile-menu" sourceId={source?.id} onEditMetadata={() => setEditor("metadata")} onEditImages={() => setEditor("images")} onEditSubtitles={() => setEditor("subtitles")} onDelete={() => setDeleteOpen(true)} onIdentify={() => setEditor("identify")} onRefreshMetadata={() => void refreshMetadata()} onScanFolder={() => void scanLibrary()} onLockMetadata={() => void setMetadataLock(true)} onUnlockMetadata={() => void setMetadataLock(false)} />
      </div>
      {backdrop ? <img className="lux-detail-backdrop" src={backdrop} alt="" /> : null}
      <div className="lux-detail-overlay" />
      <div className="lux-detail-content">
        <div className="lux-detail-grid">
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
          <div className="lux-detail-copy">
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
            <div className="lux-hero-actions">
              {watchHref ? (
                <Link className="lux-button lux-button-large lux-button-primary" to={watchHref}><Play size={17} fill="currentColor" /> 播放</Link>
              ) : (
                <MediaEpisodePlayButton item={media} onNotice={setActionNotice} />
              )}
              <button className="lux-button lux-button-large lux-button-glass lux-detail-action-control" type="button" data-action="toggle-favorite" aria-pressed={Boolean(playback.data?.isFavorite)} disabled={pendingPlaybackAction !== undefined} onClick={() => void updatePlaybackFlag("favorite")}>
                <span className="lux-detail-action-icon"><Heart size={17} fill={playback.data?.isFavorite ? "currentColor" : "none"} /></span>
                <span className="lux-detail-action-label">{playback.data?.isFavorite ? "已收藏" : "收藏"}</span>
              </button>
              {media.metadataPending || pendingReview ? (
                <button className="lux-button lux-button-large lux-button-glass" type="button" data-action="confirm-metadata" disabled={confirmingMetadata} onClick={() => void confirmMetadata()}>
                  <Check size={17} /> {confirmingMetadata ? "确认中…" : "确认元数据"}
                </button>
              ) : null}
              <button
                className={`lux-detail-watched-status${playback.data?.isPlayed ? " is-played" : ""}`}
                type="button"
                data-action="toggle-played"
                aria-pressed={Boolean(playback.data?.isPlayed)}
                aria-label={playback.data?.isPlayed ? "已看" : "标记为已看"}
                title={playback.data?.isPlayed ? "取消已看" : "标记为已看"}
                disabled={pendingPlaybackAction !== undefined}
                onClick={() => void updatePlaybackFlag("played")}
              >
                <span className="lux-detail-action-icon"><Check size={20} strokeWidth={2.4} /></span>
              </button>
              <MediaActionMenu item={media} className="lux-detail-inline-menu" showLabel sourceId={source?.id} onEditMetadata={() => setEditor("metadata")} onEditImages={() => setEditor("images")} onEditSubtitles={() => setEditor("subtitles")} onDelete={() => setDeleteOpen(true)} onIdentify={() => setEditor("identify")} onRefreshMetadata={() => void refreshMetadata()} onScanFolder={() => void scanLibrary()} onLockMetadata={() => void setMetadataLock(true)} onUnlockMetadata={() => void setMetadataLock(false)} />
              {pendingReview && nextPendingItem ? <Link className="lux-button lux-button-large lux-button-glass lux-detail-next-pending" data-action="next-pending" to={`/items/${nextPendingItem.id}?metadataStatus=pending`}>下一个待确认</Link> : null}
              {source ? <span className="lux-detail-source"><Radio size={16} /> {source.container || "DIRECT PLAY"}</span> : null}
            </div>
            {actionError ? <p className="lux-editor-error lux-detail-action-error" role="alert">{actionError}</p> : null}
            {actionNotice ? <p className="lux-muted-copy lux-detail-action-error" role="status">{actionNotice}</p> : null}
            <MediaSourcePanel />
          </div>
        </div>
        <div className="lux-detail-sections">
          <MediaEpisodesSection item={media}>
          {mediaTagline ? (
            <div className="lux-detail-tagline-wrap">
              <div className="lux-detail-tagline lux-media-nfo-tagline font-serif">
                <span className="lux-tagline-quote" aria-hidden="true">“</span>
                <span className="lux-tagline-text">{mediaTagline}</span>
                <span className="lux-tagline-quote" aria-hidden="true">”</span>
              </div>
            </div>
          ) : null}
          <div className="lux-detail-cast">
            <MediaCastSection actors={media.actors} />
          </div>
          </MediaEpisodesSection>
          {showStills ? <MediaStillsSection itemId={media.id} title={detailTitle} /> : null}
          <div className="lux-detail-nfo">
            <MediaNfoSection item={media} source={source} detailTitle={detailTitle} addedAtLabel={addedAtLabel} />
          </div>
        </div>
      </div>
      {editor === "metadata" ? <MediaMetadataEditor item={media} onClose={() => setEditor(undefined)} /> : null}
      {editor === "images" ? <MediaImageEditor item={media} onClose={() => {
        setEditor(undefined);
        void queryClient.invalidateQueries({ queryKey: queryKeys.itemImages(media.id) });
      }} /> : null}
      {editor === "subtitles" ? <MediaSubtitleEditor item={media} sourceId={source?.id} onClose={() => setEditor(undefined)} onSaved={() => void queryClient.invalidateQueries({ queryKey: queryKeys.item(media.id) })} /> : null}
      {deleteOpen ? <MediaDeleteDialog item={media} onClose={() => setDeleteOpen(false)} onConfirm={() => api.deleteItem(media.id, source?.id)} onDeleted={() => navigate("/libraries")} /> : null}
      {editor === "identify" ? <MediaIdentifier item={media} onClose={() => setEditor(undefined)} onSaved={() => { void queryClient.invalidateQueries({ queryKey: queryKeys.item(media.id) }); void queryClient.invalidateQueries({ queryKey: pendingItemsQueryKey }); }} /> : null}
    </article>
  );
}

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
