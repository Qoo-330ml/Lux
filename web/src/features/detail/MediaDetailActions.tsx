import { createContext, lazy, memo, useContext, useState, type ReactNode } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowLeft, Check, Heart, Play, Radio } from "lucide-react";
import { Link, useNavigate } from "react-router-dom";
import { api } from "../../lib/api/client";
import { queryKeys, queryRefreshIntervals } from "../../lib/api/query-keys";
import type { MediaItem } from "../../lib/api/types";
import { MediaActionMenu } from "../media/MediaActionMenu";
import { MediaEditorBoundary } from "../media/MediaEditorBoundary";
import { useMediaSource } from "./MediaSourcePanel";
import { MediaEpisodePlayButton } from "./MediaEpisodesSection";
import { streamsOfType } from "./media-source";
import { LuxLogo } from "../../components/LuxLogo";

const LazyMetadataEditor = lazy(() => import("../media/MediaMetadataEditor").then((module) => ({ default: module.MediaMetadataEditor })));
const LazyImageEditor = lazy(() => import("../media/MediaImageEditor").then((module) => ({ default: module.MediaImageEditor })));
const LazySubtitleEditor = lazy(() => import("../media/MediaSubtitleEditor").then((module) => ({ default: module.MediaSubtitleEditor })));
const LazyIdentifier = lazy(() => import("../media/MediaIdentifier").then((module) => ({ default: module.MediaIdentifier })));
const LazyDeleteDialog = lazy(() => import("../media/MediaDeleteDialog").then((module) => ({ default: module.MediaDeleteDialog })));

type EditorKind = "metadata" | "images" | "subtitles" | "identify";
type ActionContextValue = {
  item: MediaItem;
  sourceId?: string;
  watchHref?: string;
  pendingReview: boolean;
  nextPendingItem?: MediaItem;
  favorite: boolean;
  played: boolean;
  pending: boolean;
  confirmingMetadata: boolean;
  error?: string;
  notice?: string;
  setNotice: (notice?: string) => void;
  setEditor: (editor: EditorKind) => void;
  openDelete: () => void;
  refreshMetadata: () => void;
  scanLibrary: () => void;
  setMetadataLock: (locked: boolean) => void;
  confirmMetadata: () => void;
  updatePlaybackFlag: (kind: "favorite" | "played") => void;
};
const ActionContext = createContext<ActionContextValue | undefined>(undefined);

export function MediaDetailActions({ item, pendingReview, children }: {
  item: MediaItem;
  pendingReview: boolean;
  children: ReactNode;
}) {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const { source, selectedAudioStreamIndex, selectedSubtitleStreamIndex } = useMediaSource();
  const playback = useQuery({
    queryKey: queryKeys.playback(item.id),
    queryFn: () => api.playback(item.id),
    refetchInterval: queryRefreshIntervals.mediaSurface,
  });
  const pendingItemsQueryKey = queryKeys.library(item.libraryId ?? "", 1, undefined, "Name", "Ascending", "PENDING", 100);
  const pendingItems = useQuery({
    queryKey: pendingItemsQueryKey,
    queryFn: () => api.libraryItems(item.libraryId ?? "", 1, undefined, { metadataStatus: "PENDING", pageSize: 100 }),
    enabled: pendingReview && Boolean(item.libraryId),
  });
  const [editor, setEditor] = useState<EditorKind>();
  const [deleteOpen, setDeleteOpen] = useState(false);
  const [error, setError] = useState<string>();
  const [notice, setNotice] = useState<string>();
  const [confirmingMetadata, setConfirmingMetadata] = useState(false);
  const [pendingPlaybackAction, setPendingPlaybackAction] = useState<"favorite" | "played">();
  const pendingReviewItems = pendingItems.data?.items ?? [];
  const pendingIndex = pendingReviewItems.findIndex((entry) => entry.id === item.id);
  const nextPendingItem = pendingReviewItems[pendingIndex >= 0 ? pendingIndex + 1 : 0];
  const watchQuery = source ? new URLSearchParams({ sourceId: source.id }) : undefined;
  const firstAudioStreamIndex = streamsOfType(source, "AUDIO")[0]?.index;
  if (watchQuery && selectedAudioStreamIndex && selectedAudioStreamIndex !== String(firstAudioStreamIndex ?? "")) {
    watchQuery.set("audioStreamIndex", selectedAudioStreamIndex);
  }
  if (watchQuery && selectedSubtitleStreamIndex) watchQuery.set("subtitleStreamIndex", selectedSubtitleStreamIndex);
  const watchHref = source ? `/watch/${item.id}?${watchQuery?.toString()}` : undefined;

  async function setMetadataLock(locked: boolean) {
    setError(undefined);
    setNotice(undefined);
    try {
      await api.setItemMetadataLock(item.id, locked);
      await queryClient.invalidateQueries({ queryKey: queryKeys.item(item.id) });
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "元数据锁定状态更新失败，请重试。");
    }
  }

  async function refreshMetadata() {
    setError(undefined);
    setNotice(undefined);
    try {
      await api.startItemMetadataRefresh(item.id);
      setNotice("元数据刷新任务已提交，可在管理任务中查看进度。");
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "元数据刷新任务提交失败，请重试。");
    }
  }

  async function scanLibrary() {
    setError(undefined);
    setNotice(undefined);
    try {
      await api.startItemFolderScan(item.id);
      setNotice("所在文件夹扫描任务已提交，可在管理任务中查看进度。");
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "媒体库扫描任务提交失败，请重试。");
    }
  }

  async function confirmMetadata() {
    setError(undefined);
    setNotice(undefined);
    setConfirmingMetadata(true);
    try {
      const result = await api.confirmAdminMetadata([item.id]);
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: queryKeys.item(item.id) }),
        queryClient.invalidateQueries({ queryKey: ["library", item.libraryId ?? ""] }),
        queryClient.invalidateQueries({ queryKey: pendingItemsQueryKey }),
      ]);
      if (result.failedCount > 0) setError("当前待确认候选确认失败，请重新搜索候选后再试。");
      else setNotice("元数据已确认。");
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "元数据确认失败，请重试。");
    } finally {
      setConfirmingMetadata(false);
    }
  }

  async function updatePlaybackFlag(kind: "favorite" | "played") {
    if (!playback.data || pendingPlaybackAction) return;
    setError(undefined);
    setNotice(undefined);
    setPendingPlaybackAction(kind);
    try {
      if (kind === "favorite") {
        await api.setFavorite(item.id, !playback.data.isFavorite);
        setNotice(playback.data.isFavorite ? "已移出收藏。" : "已加入收藏。");
      } else {
        await api.setPlayed(item.id, !playback.data.isPlayed);
        setNotice(playback.data.isPlayed ? "已取消已看标记。" : "已标记为已看。");
      }
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: queryKeys.playback(item.id) }),
        queryClient.invalidateQueries({ queryKey: queryKeys.item(item.id) }),
        queryClient.invalidateQueries({ queryKey: queryKeys.home }),
        queryClient.invalidateQueries({ queryKey: queryKeys.favorites }),
      ]);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "播放状态更新失败，请重试。");
    } finally {
      setPendingPlaybackAction(undefined);
    }
  }

  const value: ActionContextValue = {
    item,
    sourceId: source?.id,
    watchHref,
    pendingReview,
    nextPendingItem,
    favorite: Boolean(playback.data?.isFavorite),
    played: Boolean(playback.data?.isPlayed),
    pending: pendingPlaybackAction !== undefined,
    confirmingMetadata,
    error,
    notice,
    setNotice,
    setEditor,
    openDelete: () => setDeleteOpen(true),
    refreshMetadata: () => void refreshMetadata(),
    scanLibrary: () => void scanLibrary(),
    setMetadataLock: (locked) => void setMetadataLock(locked),
    confirmMetadata: () => void confirmMetadata(),
    updatePlaybackFlag: (kind) => void updatePlaybackFlag(kind),
  };

  function closeEditor() {
    setEditor(undefined);
  }

  return <ActionContext value={value}>
    {children}
    {editor || deleteOpen ? <MediaEditorBoundary key={editor ?? "delete"} onClose={() => { closeEditor(); setDeleteOpen(false); }}>
      {editor === "metadata" ? <LazyMetadataEditor item={item} onClose={closeEditor} /> : null}
      {editor === "images" ? <LazyImageEditor item={item} onClose={() => {
        closeEditor();
        void queryClient.invalidateQueries({ queryKey: queryKeys.itemImages(item.id) });
      }} /> : null}
      {editor === "subtitles" ? <LazySubtitleEditor item={item} sourceId={source?.id} onClose={closeEditor} onSaved={() => void queryClient.invalidateQueries({ queryKey: queryKeys.item(item.id) })} /> : null}
      {deleteOpen ? <LazyDeleteDialog item={item} onClose={() => setDeleteOpen(false)} onConfirm={() => api.deleteItem(item.id, source?.id)} onDeleted={() => navigate("/libraries")} /> : null}
      {editor === "identify" ? <LazyIdentifier item={item} onClose={closeEditor} onSaved={() => {
        void queryClient.invalidateQueries({ queryKey: queryKeys.item(item.id) });
        void queryClient.invalidateQueries({ queryKey: pendingItemsQueryKey });
      }} /> : null}
    </MediaEditorBoundary> : null}
  </ActionContext>;
}

function useDetailActions() {
  const value = useContext(ActionContext);
  if (!value) throw new Error("Media detail actions require MediaDetailActions");
  return value;
}

export const MediaDetailMobileHeader = memo(function MediaDetailMobileHeader() {
  const navigate = useNavigate();
  return <div className="lux-detail-mobile-header">
    <button className="lux-detail-mobile-back" type="button" aria-label="返回上一页" onClick={() => navigate(-1)}>
      <ArrowLeft size={21} />
    </button>
    <Link className="lux-detail-mobile-brand" to="/" aria-label="Lux 首页">
      <LuxLogo className="lux-detail-mobile-brand-logo" />
      <span>Lux</span>
    </Link>
    <MediaDetailActionMenu className="lux-detail-mobile-menu" />
  </div>;
});

export const MediaDetailActionMenu = memo(function MediaDetailActionMenu({ className, showLabel }: {
  className: string;
  showLabel?: boolean;
}) {
  const actions = useDetailActions();
  const { item } = actions;
  return <MediaActionMenu
    item={item}
    className={className}
    showLabel={showLabel}
    sourceId={actions.sourceId}
    onEditMetadata={() => actions.setEditor("metadata")}
    onEditImages={() => actions.setEditor("images")}
    onEditSubtitles={() => actions.setEditor("subtitles")}
    onDelete={actions.openDelete}
    onIdentify={() => actions.setEditor("identify")}
    onRefreshMetadata={actions.refreshMetadata}
    onScanFolder={actions.scanLibrary}
    onLockMetadata={() => actions.setMetadataLock(true)}
    onUnlockMetadata={() => actions.setMetadataLock(false)}
  />;
});

export const MediaDetailActionControls = memo(function MediaDetailActionControls() {
  const actions = useDetailActions();
  return <>
    <div className="lux-hero-actions">
      {actions.watchHref ? <Link className="lux-button lux-button-large lux-button-primary" to={actions.watchHref}><Play size={17} fill="currentColor" /> 播放</Link> : <MediaEpisodePlayButton item={actions.item} onNotice={actions.setNotice} />}
      <button className="lux-button lux-button-large lux-button-glass lux-detail-action-control" type="button" data-action="toggle-favorite" aria-pressed={actions.favorite} disabled={actions.pending} onClick={() => actions.updatePlaybackFlag("favorite")}>
        <span className="lux-detail-action-icon"><Heart size={17} fill={actions.favorite ? "currentColor" : "none"} /></span>
        <span className="lux-detail-action-label">{actions.favorite ? "已收藏" : "收藏"}</span>
      </button>
      {actions.item.metadataPending || actions.pendingReview ? <button className="lux-button lux-button-large lux-button-glass" type="button" data-action="confirm-metadata" disabled={actions.confirmingMetadata} onClick={actions.confirmMetadata}>
        <Check size={17} /> {actions.confirmingMetadata ? "确认中…" : "确认元数据"}
      </button> : null}
      <button className={`lux-detail-watched-status${actions.played ? " is-played" : ""}`} type="button" data-action="toggle-played" aria-pressed={actions.played} aria-label={actions.played ? "已看" : "标记为已看"} title={actions.played ? "取消已看" : "标记为已看"} disabled={actions.pending} onClick={() => actions.updatePlaybackFlag("played")}>
        <span className="lux-detail-action-icon"><Check size={20} strokeWidth={2.4} /></span>
      </button>
      <MediaDetailActionMenu className="lux-detail-inline-menu" showLabel />
      {actions.pendingReview && actions.nextPendingItem ? <Link className="lux-button lux-button-large lux-button-glass lux-detail-next-pending" data-action="next-pending" to={`/items/${actions.nextPendingItem.id}?metadataStatus=pending`}>下一个待确认</Link> : null}
      {actions.sourceId ? <span className="lux-detail-source"><Radio size={16} /> {actions.item.mediaSources?.find((source) => source.id === actions.sourceId)?.container || "DIRECT PLAY"}</span> : null}
    </div>
    {actions.error ? <p className="lux-editor-error lux-detail-action-error" role="alert">{actions.error}</p> : null}
    {actions.notice ? <p className="lux-muted-copy lux-detail-action-error" role="status">{actions.notice}</p> : null}
  </>;
});
