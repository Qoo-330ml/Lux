import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowLeft, Check, Heart, Play, Radio } from "lucide-react";
import { useState } from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router-dom";
import { api } from "../../lib/api/client";
import { queryKeys, queryRefreshIntervals } from "../../lib/api/query-keys";
import type { MediaItem } from "../../lib/api/types";
import { MediaSourcePanel, MediaSourceProvider, useMediaSource } from "./MediaSourcePanel";
import { streamsOfType } from "./media-source";
import { MediaCastSection, MediaNfoSection, MediaStillsSection } from "./MediaDetailSections";
import { mediaTitle } from "../home/media";
import { MediaActionMenu } from "../media/MediaActionMenu";
import { MediaImageEditor } from "../media/MediaImageEditor";
import { MediaIdentifier } from "../media/MediaIdentifier";
import { MediaMetadataEditor } from "../media/MediaMetadataEditor";
import { MediaSubtitleEditor } from "../media/MediaSubtitleEditor";
import { MediaDeleteDialog } from "../media/MediaDeleteDialog";
import { MediaEpisodesSection, MediaEpisodePlayButton } from "./MediaEpisodesSection";
import { MediaDetailArtwork, MediaDetailHero } from "./MediaDetailHero";
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
  const detailKind = ({ VIDEO: "video", SERIES: "series", SEASON: "season", EPISODE: "episode" } as Record<string, string>)[media.itemType ?? ""] ?? "movie";
  const showStills = media.itemType === "MOVIE" || media.itemType === "SERIES";
  const mediaTagline = media.nfo?.tagline?.trim();
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
      <MediaDetailArtwork media={media} placement="backdrop" />
      <div className="lux-detail-overlay" />
      <div className="lux-detail-content">
        <div className="lux-detail-grid">
          <MediaDetailArtwork media={media} placement="poster" />
          <div className="lux-detail-copy">
            <MediaDetailHero media={media} collectionId={collectionId} />
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
          {showStills ? <MediaStillsSection itemId={media.id} title={mediaTitle(media)} /> : null}
          <div className="lux-detail-nfo">
            <MediaNfoSection item={media} source={source} />
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
