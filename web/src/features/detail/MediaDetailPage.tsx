import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Check, Heart, Play, Radio, Sparkles, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Link, useNavigate, useParams } from "react-router-dom";
import { LuxSelect } from "../../components/LuxSelect";
import { api } from "../../lib/api/client";
import { queryKeys, queryRefreshIntervals } from "../../lib/api/query-keys";
import type { MediaItem, MediaSource, MediaStream } from "../../lib/api/types";
import { MediaInfoPanel } from "./MediaInfoPanel";
import { MediaCast } from "./MediaCast";
import { EpisodeCount, Rating, episodeTitle, imageUrl, mediaTitle, runtimeLabel } from "../home/media";
import { MediaActionMenu } from "../media/MediaActionMenu";
import { MediaImageEditor } from "../media/MediaImageEditor";
import { MediaIdentifier } from "../media/MediaIdentifier";
import { MediaMetadataEditor } from "../media/MediaMetadataEditor";
import { MediaSubtitleEditor } from "../media/MediaSubtitleEditor";
import { MediaDeleteDialog } from "../media/MediaDeleteDialog";

export function MediaDetailPage() {
  const { itemId = "" } = useParams();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const item = useQuery({
    queryKey: queryKeys.item(itemId),
    queryFn: () => api.item(itemId),
    enabled: Boolean(itemId),
    refetchInterval: queryRefreshIntervals.mediaSurface,
  });
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
  const [selectedSourceId, setSelectedSourceId] = useState<string>();
  const [editor, setEditor] = useState<"metadata" | "images" | "subtitles" | "identify">();
  const [actionError, setActionError] = useState<string>();
  const [actionNotice, setActionNotice] = useState<string>();
  const [deleteOpen, setDeleteOpen] = useState(false);
  const isSeries = item.data?.itemType === "SERIES";
  const isSeason = item.data?.itemType === "SEASON";
  const isEpisode = item.data?.itemType === "EPISODE";
  const seasons = useQuery({
    queryKey: queryKeys.children(itemId, "SEASON"),
    queryFn: () => api.children(itemId, { itemType: "SEASON" }),
    enabled: isSeries,
    refetchInterval: queryRefreshIntervals.mediaSurface,
  });
  const activeSeasonId = seasons.data?.items?.[0]?.id;
  const hierarchySeriesId = item.data && !isSeries
    ? item.data.seriesId ?? item.data.parentId ?? undefined
    : undefined;
  const seriesContext = useQuery({
    queryKey: queryKeys.item(hierarchySeriesId ?? ""),
    queryFn: () => api.item(hierarchySeriesId ?? ""),
    enabled: Boolean(hierarchySeriesId),
    refetchInterval: queryRefreshIntervals.mediaSurface,
  });
  const episodeSeriesId = isSeries ? itemId : hierarchySeriesId;
  const episodeSeasonId = isSeries
    ? activeSeasonId
    : isSeason
      ? item.data?.id
      : isEpisode
        ? item.data?.parentId ?? undefined
        : undefined;
  const episodes = useQuery({
    queryKey: queryKeys.children(episodeSeriesId ?? itemId, "EPISODE", episodeSeasonId),
    queryFn: () => api.children(episodeSeriesId ?? itemId, { itemType: "EPISODE", seasonId: episodeSeasonId }),
    enabled: Boolean(episodeSeriesId) && Boolean(episodeSeasonId) && Boolean(isSeries || isSeason || isEpisode),
    refetchInterval: queryRefreshIntervals.mediaSurface,
  });

  useEffect(() => {
    setSelectedSourceId(undefined);
  }, [itemId]);

  if (item.isPending) return <section className="lux-page-state"><p>正在加载媒体详情…</p></section>;
  if (item.error) return <section className="lux-page-state"><h1>媒体详情加载失败</h1><p>{item.error.message}</p></section>;

  const media = item.data;
  const logo = itemImages.data?.images?.find((image) => image.imageType.toUpperCase() === "LOGO");
  const backdrop = imageUrl(media, "fanart") ?? imageUrl(media);
  const poster = imageUrl(media);
  const detailKind = isSeries ? "series" : isSeason ? "season" : isEpisode ? "episode" : "movie";
  const detailTitle = isSeries || (!isSeason && !isEpisode)
    ? mediaTitle(media)
    : mediaTitle(seriesContext.data ?? media);
  const detailOriginalTitle = media.originalTitle?.trim();
  const tmdbId = providerId(media.providerIds, "tmdb");
  const detailSubtitle = isSeason
    ? `第 ${media.parentIndexNumber ?? ""} 季`
    : isEpisode
      ? episodeTitle(media)
      : undefined;
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
    <article className={`lux-detail-page lux-detail-page-${detailKind}`}>
      {backdrop ? <img className="lux-detail-backdrop" src={backdrop} alt="" /> : null}
      <div className="lux-detail-overlay" />
      <div className="lux-detail-content">
        <div className="lux-detail-grid">
          <div className="lux-detail-poster-column">
            <div className={`lux-detail-poster${isEpisode ? " is-landscape" : ""}`}>
              {isEpisode && backdrop
                ? <img src={backdrop} alt={`${mediaTitle(media)} 剧照`} />
                : poster
                  ? <img src={poster} alt={`${mediaTitle(media)} 海报`} />
                  : <span><Sparkles size={32} />{mediaTitle(media)}</span>}
            </div>
          </div>
          <div className="lux-detail-copy">
            <div className="lux-detail-title-row">
              {logo ? <img className="lux-detail-logo" src={logo.url} alt={`${mediaTitle(media)} 徽标`} /> : null}
              <h1>{detailTitle}</h1>
              {detailOriginalTitle && detailOriginalTitle !== detailTitle
                ? <p className="lux-detail-original-title">{detailOriginalTitle}</p>
                : null}
              {detailSubtitle ? <p className="lux-detail-subtitle">{detailSubtitle}</p> : null}
            </div>
            <div className="lux-detail-meta">
              {media.premiereDate
                ? <span>首播 {media.premiereDate}</span>
                : media.productionYear
                  ? <span>{media.productionYear}</span>
                  : null}
              {isSeries && media.seasonCount != null ? <span>{media.seasonCount} 季</span> : null}
              {isSeries && media.episodeCount != null ? <span>{media.episodeCount} 集</span> : null}
              {tmdbId ? <span>TMDb {tmdbId}</span> : null}
              {media.rating != null && Number.isFinite(media.rating)
                ? <span>{media.ratingSource ? `${media.ratingSource} 评分` : "评分"} {media.rating.toFixed(1)}</span>
                : null}
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
              <MediaActionMenu item={media} sourceId={source?.id} onEditMetadata={() => setEditor("metadata")} onEditImages={() => setEditor("images")} onEditSubtitles={() => setEditor("subtitles")} onDelete={() => setDeleteOpen(true)} onIdentify={() => setEditor("identify")} onRefreshMetadata={() => void refreshMetadata()} onScanLibrary={() => void scanLibrary()} onLockMetadata={() => void setMetadataLock(true)} onUnlockMetadata={() => void setMetadataLock(false)} />
              {source ? <span className="lux-detail-source"><Radio size={16} /> {source.container || "DIRECT PLAY"}</span> : null}
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
        {source ? (
          <MediaInfoPanel
            source={source}
            itemType={media.itemType}
            lastAirDate={media.lastAirDate}
            status={media.status}
            originalLanguage={media.originalLanguage}
          />
        ) : null}
        {isSeries ? (
          <SeriesChildren
            seasons={seasons.data?.items ?? []}
          />
        ) : null}
        {isSeason ? <SeasonEpisodes episodes={episodes.data?.items ?? []} seasonNumber={media.parentIndexNumber} episodesPending={episodes.isPending} /> : null}
        {isEpisode ? <EpisodeRail episodes={episodes.data?.items ?? []} currentEpisodeId={media.id} seasonNumber={media.parentIndexNumber} episodesPending={episodes.isPending} /> : null}
      </div>
      {editor === "metadata" ? <MediaMetadataEditor item={media} onClose={() => setEditor(undefined)} /> : null}
      {editor === "images" ? <MediaImageEditor item={media} onClose={() => {
        setEditor(undefined);
        void queryClient.invalidateQueries({ queryKey: queryKeys.itemImages(media.id) });
      }} /> : null}
      {editor === "subtitles" ? <MediaSubtitleEditor item={media} sourceId={source?.id} onClose={() => setEditor(undefined)} onSaved={() => void queryClient.invalidateQueries({ queryKey: queryKeys.item(media.id) })} /> : null}
      {deleteOpen ? <MediaDeleteDialog item={media} onClose={() => setDeleteOpen(false)} onConfirm={() => api.deleteItem(media.id, source?.id)} onDeleted={() => navigate("/libraries")} /> : null}
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
  const options = sources.map((source, index) => ({
    value: source.id,
    label: (
      <span className="lux-source-option-content">
        <span className="lux-source-option-label">{sourceLabel(source, index)}</span>
        <span className="lux-source-option-detail">{source.editionName || source.container || "DIRECT PLAY"}</span>
      </span>
    ),
  }));

  return (
    <section className="lux-source-selector" aria-labelledby="media-source-heading">
      <div className="lux-section-heading">
        <h2 id="media-source-heading">选择版本</h2>
        <span>{sources.length} 个视频文件</span>
      </div>
      <div className="lux-source-select">
        <LuxSelect
          value={selectedSourceId ?? sources[0]?.id ?? ""}
          options={options}
          onChange={onSelect}
          aria-labelledby="media-source-heading"
        />
      </div>
    </section>
  );
}

function sourceLabel(source: MediaSource, index: number) {
  const videoStream = source.streams?.find((stream) => (stream.type ?? "").toUpperCase() === "VIDEO");
  const qualityTokens = splitSourceInfo(source.qualityLabel);
  const qualityRange = qualityTokens.find(dynamicRangeToken);
  const labels = uniqueSourceLabels([
    ...qualityTokens.filter((token) => !dynamicRangeToken(token)),
    videoCodecLabel(videoStream),
    dynamicRangeLabel(videoStream) ?? qualityRange,
    bitDepthLabel(videoStream),
  ]);
  return labels.join(" · ") || source.editionName || `${source.container?.toUpperCase() || "视频"} · 版本 ${index + 1}`;
}

function splitSourceInfo(value?: string | null) {
  return value?.split(/\s*[·•|/,]\s*|\s+/).map((part) => part.trim()).filter(Boolean) ?? [];
}

function dynamicRangeToken(value: string) {
  const normalized = value.toLowerCase().replace(/[\s_-]+/g, "");
  if (normalized === "sdr") return "SDR";
  if (normalized === "hdr10+") return "HDR10+";
  if (normalized === "hdr10") return "HDR10";
  if (normalized === "hdr") return "HDR";
  if (normalized === "hlg") return "HLG";
  if (normalized === "dv" || normalized === "dovi" || normalized === "dolbyvision") return "Dolby Vision";
  return undefined;
}

function videoCodecLabel(stream?: MediaStream) {
  const codec = stream?.codec?.trim();
  if (!codec) return undefined;
  const normalized = codec.toLowerCase().replace(/[^a-z0-9]+/g, "");
  if (normalized === "hevc" || normalized === "h265" || normalized === "x265") return "HEVC";
  if (normalized === "h264" || normalized === "avc" || normalized === "x264") return "H.264";
  if (normalized === "mpeg4") return "MPEG-4";
  if (normalized === "av1") return "AV1";
  if (normalized === "vp9") return "VP9";
  if (normalized === "vp8") return "VP8";
  return codec.toUpperCase();
}

function dynamicRangeLabel(stream?: MediaStream) {
  if (!stream) return undefined;
  const detailText = Object.entries(stream.details ?? {})
    .filter(([key]) => /videorange|extendedvideotype|extendedvideosubtype|colortransfer|colorprimaries/i.test(key))
    .map(([, value]) => String(value))
    .join(" ");
  const value = `${stream.title ?? ""} ${detailText}`.toLowerCase().replace(/[_-]+/g, " ");
  if (/\bdolby\s+vision\b|\bdovi\b|\bdv\b/.test(value)) return "Dolby Vision";
  if (/\bhdr10\+?\b|\bhdr10plus\b/.test(value)) return value.includes("hdr10+") || value.includes("hdr10plus") ? "HDR10+" : "HDR10";
  if (/\bhdr\b|\bsmpte\s*2084\b|\bpq\b/.test(value)) return "HDR";
  if (/\bhlg\b|arib\s+std\s+b67/.test(value)) return "HLG";
  if (/\bsdr\b/.test(value)) return "SDR";
  return undefined;
}

function bitDepthLabel(stream?: MediaStream) {
  const rawValue = stream?.details?.BitDepth;
  if (rawValue === undefined || rawValue === null) return undefined;
  const match = String(rawValue).match(/\d+/);
  const bitDepth = match ? Number(match[0]) : Number.NaN;
  return Number.isFinite(bitDepth) && bitDepth > 0 ? `${bitDepth}-bit` : undefined;
}

function uniqueSourceLabels(labels: Array<string | undefined>) {
  const seen = new Set<string>();
  return labels.filter((label): label is string => {
    const trimmed = label?.trim();
    if (!trimmed) return false;
    const key = trimmed.toLowerCase().replace(/[\s.·-]+/g, "");
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

function providerId(providerIds: Record<string, string> | null | undefined, provider: string) {
  return Object.entries(providerIds ?? {}).find(([name]) => name.toLowerCase() === provider.toLowerCase())?.[1];
}

function SeriesChildren({
  seasons,
}: {
  seasons: MediaItem[];
}) {
  return (
    <section className="lux-series-children" aria-labelledby="series-children-heading">
      <div className="lux-section-heading">
        <h2 id="series-children-heading">播出季</h2>
        <span>{seasons.length} 个季度</span>
      </div>
      <div className="lux-season-rail" role="list" aria-label="播出季">
        {seasons.map((season) => (
          <Link
            className="lux-season-card"
            key={season.id}
            role="listitem"
            to={`/items/${season.id}`}
          >
            <span className="lux-season-card-art">
              {imageUrl(season) ? <img src={imageUrl(season)} alt={`${mediaTitle(season)} 海报`} loading="lazy" /> : <span className="lux-season-card-placeholder">{mediaTitle(season)}</span>}
              <Rating value={season.rating} source={season.ratingSource} placement="card" />
              <EpisodeCount item={season} />
            </span>
            <strong>{mediaTitle(season)}</strong>
          </Link>
        ))}
      </div>
    </section>
  );
}

function SeasonEpisodes({ episodes, seasonNumber, episodesPending }: { episodes: MediaItem[]; seasonNumber?: number | null; episodesPending: boolean }) {
  return (
    <section className="lux-season-episodes" aria-labelledby="season-episodes-heading">
      <div className="lux-section-heading">
        <h2 id="season-episodes-heading">单集</h2>
        <span>{episodes.length} 集</span>
      </div>
      {episodesPending ? <p className="lux-muted-copy">正在加载单集…</p> : null}
      {!episodesPending && episodes.length ? (
        <div className="lux-season-episode-list" role="list">
          {episodes.map((episode, index) => <SeasonEpisodeRow episode={episode} seasonNumber={seasonNumber} fallbackNumber={index + 1} key={episode.id} />)}
        </div>
      ) : null}
      {!episodesPending && !episodes.length ? <p className="lux-muted-copy">这个季度还没有可播放的单集。</p> : null}
    </section>
  );
}

function SeasonEpisodeRow({ episode, seasonNumber, fallbackNumber }: { episode: MediaItem; seasonNumber?: number | null; fallbackNumber: number }) {
  const image = imageUrl(episode, "fanart") ?? imageUrl(episode);
  const number = episode.indexNumber ?? fallbackNumber;
  return (
    <Link className="lux-season-episode-row" role="listitem" to={`/items/${episode.id}`}>
      <span className="lux-season-episode-thumb">
        {image ? <img src={image} alt="" loading="lazy" /> : <span>{mediaTitle(episode)}</span>}
      </span>
      <span className="lux-season-episode-copy">
        <strong>{episodeTitle(episode, seasonNumber, number)}</strong>
        {episode.productionYear ? <small>{episode.productionYear}</small> : null}
        {episode.overview ? <p>{episode.overview}</p> : null}
      </span>
      <span className="lux-season-episode-arrow" aria-hidden="true">查看详情 →</span>
    </Link>
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
        <h2 id="episode-rail-heading">更多来自第 {seasonNumber ?? ""} 季</h2>
        <span>{otherEpisodes.length} 集</span>
      </div>
      <div className="lux-episode-card-rail" role="list">
        {otherEpisodes.map((episode) => {
          const image = imageUrl(episode, "fanart") ?? imageUrl(episode);
          return (
            <Link className="lux-episode-card" role="listitem" key={episode.id} to={`/items/${episode.id}`}>
              <span className="lux-episode-card-art">
                {image ? <img src={image} alt="" loading="lazy" /> : <span>{mediaTitle(episode)}</span>}
              </span>
              <strong>{episodeTitle(episode, seasonNumber)}</strong>
            </Link>
          );
        })}
      </div>
    </section>
  );
}
