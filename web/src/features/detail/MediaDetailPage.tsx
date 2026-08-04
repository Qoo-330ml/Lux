import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Check, Heart, Play, Radio, Sparkles, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Link, useParams } from "react-router-dom";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { MediaItem, MediaSource } from "../../lib/api/types";
import { MediaInfoPanel } from "./MediaInfoPanel";
import { MediaCast } from "./MediaCast";
import { imageUrl, mediaTitle, mediaTypeLabel, runtimeLabel } from "../home/media";
import { MediaActionMenu } from "../media/MediaActionMenu";
import { MediaImageEditor } from "../media/MediaImageEditor";
import { MediaIdentifier } from "../media/MediaIdentifier";
import { MediaMetadataEditor } from "../media/MediaMetadataEditor";

export function MediaDetailPage() {
  const { itemId = "" } = useParams();
  const queryClient = useQueryClient();
  const item = useQuery({ queryKey: queryKeys.item(itemId), queryFn: () => api.item(itemId), enabled: Boolean(itemId) });
  const itemImages = useQuery({ queryKey: queryKeys.itemImages(itemId), queryFn: () => api.itemImages(itemId), enabled: Boolean(itemId) });
  const playback = useQuery({ queryKey: queryKeys.playback(itemId), queryFn: () => api.playback(itemId), enabled: Boolean(itemId) });
  const [selectedSourceId, setSelectedSourceId] = useState<string>();
  const [selectedSeasonId, setSelectedSeasonId] = useState<string>();
  const [editor, setEditor] = useState<"metadata" | "images" | "identify">();
  const [actionError, setActionError] = useState<string>();
  const [actionNotice, setActionNotice] = useState<string>();
  const isSeries = item.data?.itemType === "SERIES";
  const seasons = useQuery({
    queryKey: queryKeys.children(itemId, "SEASON"),
    queryFn: () => api.children(itemId, { itemType: "SEASON" }),
    enabled: isSeries,
  });
  const activeSeasonId = selectedSeasonId ?? seasons.data?.items?.[0]?.id;
  const episodes = useQuery({
    queryKey: queryKeys.children(itemId, "EPISODE", activeSeasonId),
    queryFn: () => api.children(itemId, { itemType: "EPISODE", seasonId: activeSeasonId }),
    enabled: isSeries && Boolean(activeSeasonId),
  });

  useEffect(() => {
    setSelectedSourceId(undefined);
    setSelectedSeasonId(undefined);
  }, [itemId]);

  if (item.isPending) return <section className="lux-page-state"><p>正在加载媒体详情…</p></section>;
  if (item.error) return <section className="lux-page-state"><h1>媒体详情加载失败</h1><p>{item.error.message}</p></section>;

  const media = item.data;
  const logo = itemImages.data?.images?.find((image) => image.imageType.toUpperCase() === "LOGO");
  const backdrop = imageUrl(media, "fanart") ?? imageUrl(media);
  const poster = imageUrl(media);
  const sources = media.mediaSources ?? [];
  const source = sources.find((entry) => entry.id === selectedSourceId)
    ?? sources.find((entry) => entry.isDefault)
    ?? sources[0];
  const watchHref = source
    ? `/watch/${media.id}?sourceId=${encodeURIComponent(source.id)}`
    : `/watch/${media.id}`;

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

  async function scanLibrary() {
    setActionError(undefined);
    setActionNotice(undefined);
    try {
      await api.startItemLibraryScan(media.id);
      setActionNotice("媒体库扫描任务已提交，可在管理任务中查看进度。");
    } catch (cause) {
      setActionError(cause instanceof Error ? cause.message : "媒体库扫描任务提交失败，请重试。");
    }
  }

  return (
    <article className="lux-detail-page">
      {backdrop ? <img className="lux-detail-backdrop" src={backdrop} alt="" /> : null}
      <div className="lux-detail-overlay" />
      <div className="lux-detail-content">
        <div className="lux-detail-grid">
          <div className="lux-detail-poster">{poster ? <img src={poster} alt={`${mediaTitle(media)} 海报`} /> : <span><Sparkles size={32} />{mediaTitle(media)}</span>}</div>
          <div className="lux-detail-copy">
            <div className="lux-detail-title-row">
              {logo ? <img className="lux-detail-logo" src={logo.url} alt={`${mediaTitle(media)} 徽标`} /> : null}
              <h1>{mediaTitle(media)}</h1>
            </div>
            <div className="lux-detail-meta">
              {media.productionYear ? <span>{media.productionYear}</span> : null}
              {runtimeLabel(media.runtimeTicks) ? <span>{runtimeLabel(media.runtimeTicks)}</span> : null}
              {source?.qualityLabel ? <span>{source.qualityLabel}</span> : null}
            </div>
            <ExpandableOverview overview={media.overview || "暂无简介。"} />
            <div className="lux-hero-actions">
              <Link className="lux-button lux-button-primary" to={watchHref}><Play size={17} fill="currentColor" /> 播放</Link>
              <button className="lux-button lux-button-glass" type="button"><Heart size={17} /> {playback.data?.isFavorite ? "已收藏" : "收藏"}</button>
              <span
                className={`lux-detail-watched-status${playback.data?.isPlayed ? " is-played" : ""}`}
                role="img"
                aria-label={playback.data?.isPlayed ? "已看" : "未看"}
                title={playback.data?.isPlayed ? "已看" : "未看"}
              >
                <Check size={20} strokeWidth={2.4} />
              </span>
              <MediaActionMenu item={media} sourceId={source?.id} onEditMetadata={() => setEditor("metadata")} onEditImages={() => setEditor("images")} onIdentify={() => setEditor("identify")} onRefreshMetadata={() => void refreshMetadata()} onScanLibrary={() => void scanLibrary()} onLockMetadata={() => void setMetadataLock(true)} onUnlockMetadata={() => void setMetadataLock(false)} />
              <span className="lux-detail-source"><Radio size={16} /> {source ? source.container || "DIRECT PLAY" : "暂无可播放版本"}</span>
            </div>
            {actionError ? <p className="lux-editor-error lux-detail-action-error" role="alert">{actionError}</p> : null}
            {actionNotice ? <p className="lux-muted-copy lux-detail-action-error" role="status">{actionNotice}</p> : null}
            {sources.length > 1 ? (
              <MediaSourceSelector
                sources={sources}
                selectedSourceId={source?.id}
                onSelect={setSelectedSourceId}
              />
            ) : null}
          </div>
        </div>
        <MediaCast actors={media.actors ?? []} />
        {source ? <MediaInfoPanel source={source} itemType={media.itemType} /> : null}
        {isSeries ? (
          <SeriesChildren
            seasons={seasons.data?.items ?? []}
            activeSeasonId={activeSeasonId}
            onSelectSeason={setSelectedSeasonId}
            episodes={episodes.data?.items ?? []}
            episodesPending={episodes.isPending}
          />
        ) : null}
      </div>
      {editor === "metadata" ? <MediaMetadataEditor item={media} onClose={() => setEditor(undefined)} /> : null}
      {editor === "images" ? <MediaImageEditor item={media} onClose={() => {
        setEditor(undefined);
        void queryClient.invalidateQueries({ queryKey: queryKeys.itemImages(media.id) });
      }} /> : null}
      {editor === "identify" ? <MediaIdentifier item={media} onClose={() => setEditor(undefined)} onSaved={() => void queryClient.invalidateQueries({ queryKey: queryKeys.item(media.id) })} /> : null}
    </article>
  );
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
                <span className="lux-eyebrow">DETAIL</span>
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

function MediaSourceSelector({
  sources,
  selectedSourceId,
  onSelect,
}: {
  sources: MediaSource[];
  selectedSourceId?: string;
  onSelect: (sourceId: string) => void;
}) {
  return (
    <section className="lux-source-selector" aria-labelledby="media-source-heading">
      <div className="lux-section-heading">
        <h2 id="media-source-heading">选择版本</h2>
        <span>{sources.length} 个视频文件</span>
      </div>
      <div className="lux-source-options" role="radiogroup" aria-label="选择播放版本">
        {sources.map((source, index) => {
          const selected = source.id === selectedSourceId;
          return (
            <button
              className={selected ? "lux-source-option is-selected" : "lux-source-option"}
              key={source.id}
              type="button"
              role="radio"
              aria-checked={selected}
              onClick={() => onSelect(source.id)}
            >
              <span className="lux-source-option-label">{sourceLabel(source, index)}</span>
              <span className="lux-source-option-detail">{source.editionName || source.container || "DIRECT PLAY"}</span>
            </button>
          );
        })}
      </div>
    </section>
  );
}

function sourceLabel(source: MediaSource, index: number) {
  const variant = source.qualityLabel || source.editionName;
  return variant || `${source.container?.toUpperCase() || "视频"} · 版本 ${index + 1}`;
}

function SeriesChildren({
  seasons,
  activeSeasonId,
  onSelectSeason,
  episodes,
  episodesPending,
}: {
  seasons: MediaItem[];
  activeSeasonId?: string;
  onSelectSeason: (seasonId: string) => void;
  episodes: MediaItem[];
  episodesPending: boolean;
}) {
  return (
    <section className="lux-series-children" aria-labelledby="series-children-heading">
      <div className="lux-section-heading">
        <h2 id="series-children-heading">季度与单集</h2>
        <span>{seasons.length} 个季度</span>
      </div>
      <div className="lux-season-tabs" role="tablist" aria-label="选择季度">
        {seasons.map((season) => (
          <button
            className={season.id === activeSeasonId ? "lux-season-tab is-active" : "lux-season-tab"}
            key={season.id}
            type="button"
            role="tab"
            aria-selected={season.id === activeSeasonId}
            onClick={() => onSelectSeason(season.id)}
          >
            {mediaTitle(season)}
          </button>
        ))}
      </div>
      {episodesPending ? <p className="lux-muted-copy">正在加载单集…</p> : null}
      {!episodesPending && episodes.length ? (
        <div className="lux-episode-list" role="list">
          {episodes.map((episode) => (
            <Link className="lux-episode-link" key={episode.id} role="listitem" to={`/items/${episode.id}`}>
              <span><strong>{mediaTitle(episode)}</strong><small>{mediaTypeLabel(episode.itemType)}</small></span>
              <span aria-hidden="true">查看详情 →</span>
            </Link>
          ))}
        </div>
      ) : null}
      {!episodesPending && !episodes.length ? <p className="lux-muted-copy">这个季度还没有可播放的单集。</p> : null}
    </section>
  );
}
