import { Database, MoreHorizontal, Play, ScanLine, ScanSearch } from "lucide-react";
import { createPortal } from "react-dom";
import { Link } from "react-router-dom";
import { memo, useCallback, useMemo, useRef, useState, type MouseEvent } from "react";
import { HorizontalScrollRail } from "../../components/layout/HorizontalScrollRail";
import { api } from "../../lib/api/client";
import { GLOBAL_COLLECTION_LIBRARY_ID } from "../../lib/api/types";
import type { Library, MediaItem } from "../../lib/api/types";
import { MediaCardEditors } from "./media-card-editors";
import { MediaCardVisual, MediaCardCopy } from "./media-card-visual";
import { useImageVariants } from "./image-variants";
import { MediaCardOverlay } from "./media-card-overlay";
import { usePopoverPosition } from "../media/usePopoverPosition";
import type { MediaCardEditor } from "./media-card-editors";
import { EpisodeCount, MediaPlaceholder, episodeTitle, imageUrl, mediaTitle, mediaTypeLabel, playbackProgress, remainingRuntimeLabel } from "./media-display";
export { EpisodeCount, Rating, episodeTitle, imageUrl, mediaTitle, mediaTypeLabel, playbackPositionTicks, playbackProgress, posterUrlWithFallback, remainingRuntimeLabel, runtimeLabel } from "./media-display";

export const MediaCard = memo(function MediaCard({ item, landscape = false, metadataAttention = false, detailSearch, selectionMode = false, selected = false, folderHref, detailHref: detailHrefOverride, readOnly = false, onSelectionChange }: { item: MediaItem; landscape?: boolean; metadataAttention?: boolean; detailSearch?: string; selectionMode?: boolean; selected?: boolean; folderHref?: string; detailHref?: string; readOnly?: boolean; onSelectionChange?: (itemId: string, selected: boolean) => void }) {
  const [editor, setEditor] = useState<"metadata" | "images" | "subtitles" | "identify">();
  const [actionError, setActionError] = useState<string>();
  const [actionNotice, setActionNotice] = useState<string>();
  const [deleteOpen, setDeleteOpen] = useState(false);
  const [deleted, setDeleted] = useState(false);

  const setMetadataLock = useCallback(async (locked: boolean) => {
    setActionError(undefined);
    setActionNotice(undefined);
    try {
      await api.setItemMetadataLock(item.id, locked);
    } catch (cause) {
      setActionError(cause instanceof Error ? cause.message : "元数据锁定状态更新失败，请重试。");
    }
  }, [item.id]);

  const refreshMetadata = useCallback(async () => {
    setActionError(undefined);
    setActionNotice(undefined);
    try {
      await api.startItemMetadataRefresh(item.id);
      setActionNotice("刷新任务已提交");
    } catch (cause) {
      setActionError(cause instanceof Error ? cause.message : "元数据刷新任务提交失败，请重试。");
    }
  }, [item.id]);

  const scanLibrary = useCallback(async () => {
    setActionError(undefined);
    setActionNotice(undefined);
    try {
      await api.startItemFolderScan(item.id);
      setActionNotice("所在文件夹扫描任务已提交");
    } catch (cause) {
      setActionError(cause instanceof Error ? cause.message : "媒体库扫描任务提交失败，请重试。");
    }
  }, [item.id]);

  const detailHref = detailHrefOverride ?? `/items/${item.id}${detailSearch ?? ""}`;
  const openEditor = useCallback((next: MediaCardEditor) => setEditor(next), []);
  const openDeleteDialog = useCallback(() => setDeleteOpen(true), []);
  const closeEditor = useCallback(() => setEditor(undefined), []);
  const closeDeleteDialog = useCallback(() => setDeleteOpen(false), []);
  const markDeleted = useCallback(() => setDeleted(true), []);
  const refresh = useCallback(() => void refreshMetadata(), [refreshMetadata]);
  const scan = useCallback(() => void scanLibrary(), [scanLibrary]);
  const lockMetadata = useCallback(() => void setMetadataLock(true), [setMetadataLock]);
  const unlockMetadata = useCallback(() => void setMetadataLock(false), [setMetadataLock]);
  const visual = useMemo(() => <MediaCardVisual item={item} landscape={landscape} href={item.itemType === "FOLDER" ? folderHref : detailHref} />, [detailHref, folderHref, item, landscape]);
  if (deleted) return null;

  return (
    <>
      <article className={[landscape ? "lux-media-card lux-media-card-landscape" : "lux-media-card", selected ? "is-selected" : ""].filter(Boolean).join(" ")}>
        <MediaCardOverlay item={item} selectionMode={selectionMode} selected={selected} readOnly={readOnly} onSelectionChange={onSelectionChange} onEditorChange={openEditor} onDelete={openDeleteDialog} onRefreshMetadata={refresh} onScanFolder={scan} onLockMetadata={lockMetadata} onUnlockMetadata={unlockMetadata}>
          {visual}
        </MediaCardOverlay>
        <MediaCardCopy item={item} metadataAttention={metadataAttention} href={item.itemType === "FOLDER" ? folderHref : detailHref} />
        {actionNotice ? <p className="lux-muted-copy lux-card-action-error" role="status">{actionNotice}</p> : null}
        {actionError ? <p className="lux-editor-error lux-card-action-error" role="alert">{actionError}</p> : null}
      </article>
      <MediaCardEditors item={item} editor={editor} deleteOpen={deleteOpen} onClose={closeEditor} onDeleteClose={closeDeleteDialog} onDeleted={markDeleted} />
    </>
  );
});

export function ContinueWatchingRail({ items, total }: { items: MediaItem[]; total?: number }) {
  if (!items.length) return null;
  const itemCount = total ?? items.length;
  return (
    <section className="lux-section" aria-labelledby="continue-watching-heading">
      <div className="lux-section-heading"><h2 id="continue-watching-heading">继续观看</h2><span>{itemCount} 项</span></div>
      <HorizontalScrollRail className="lux-home-rail" ariaLabel="继续观看">
        <div className="lux-media-rail lux-continue-rail">
          {items.map((item) => <ContinueWatchingCard item={item} key={item.id} />)}
        </div>
      </HorizontalScrollRail>
    </section>
  );
}

function ContinueWatchingCard({ item }: { item: MediaItem }) {
  const { enabled } = useImageVariants();
  const image = imageUrl(item, "fanart", enabled ? 360 : undefined)
    ?? imageUrl(item, "poster", enabled ? 360 : undefined);
  const progress = playbackProgress(item);
  const remaining = remainingRuntimeLabel(item);
  const title = item.itemType === "EPISODE" && item.seriesName
    ? item.seriesName
    : mediaTitle(item);
  const subtitle = item.itemType === "EPISODE" && item.seriesName
    ? episodeTitle(item).replace(" · ", "·")
    : item.itemType === "MOVIE" && item.productionYear != null
      ? String(item.productionYear)
      : mediaTypeLabel(item.itemType);
  const recency = recentPlaybackLabel(item.userData?.lastPlayedAt);
  const accessibleLabel = `继续播放 ${title}，${subtitle}${remaining ? `，还剩 ${remaining}` : ""}${recency ? `，${recency}` : ""}`;
  return (
    <Link className="lux-continue-card" to={`/watch/${item.id}`} aria-label={accessibleLabel}>
      <div className="lux-media-art">
        {image ? <img src={image} alt="" loading="lazy" decoding="async" /> : <MediaPlaceholder item={item} />}
        <EpisodeCount item={item} />
        <span className="lux-media-hover-play" aria-hidden="true"><Play size={22} fill="currentColor" /></span>
        {progress > 0 && progress < 100 ? <span className="lux-progress"><span style={{ width: `${progress}%` }} /></span> : null}
      </div>
      <div className="lux-continue-copy">
        <div className="lux-continue-primary">
          <strong>{title}</strong>
          {remaining ? <span className="lux-continue-remaining">还剩 {remaining}</span> : null}
        </div>
        <small className="lux-continue-context">{subtitle}</small>
        {recency ? <small className="lux-continue-recency">{recency}</small> : null}
      </div>
    </Link>
  );
}

export function recentPlaybackLabel(lastPlayedAt?: number | null, now = Date.now()) {
  if (lastPlayedAt == null || !Number.isSafeInteger(lastPlayedAt) || lastPlayedAt <= 0) return undefined;
  const playedAt = new Date(lastPlayedAt * 1000);
  const elapsed = now - playedAt.getTime();
  if (!Number.isFinite(elapsed) || elapsed < 0) return undefined;

  const minute = 60_000;
  const hour = 60 * minute;
  if (elapsed < 5 * minute) return "刚才看到这里";
  if (elapsed < hour) {
    const minutes = Math.floor(elapsed / minute);
    return minutes >= 28 && minutes <= 32
      ? "半小时前看到这里"
      : `${minutes} 分钟前看到这里`;
  }

  const today = new Date(now);
  const playedDay = Math.floor(Date.UTC(playedAt.getFullYear(), playedAt.getMonth(), playedAt.getDate()) / 86_400_000);
  const todayDay = Math.floor(Date.UTC(today.getFullYear(), today.getMonth(), today.getDate()) / 86_400_000);
  const daysAgo = todayDay - playedDay;
  if (daysAgo === 0) {
    if (elapsed >= 6 * hour) {
      const period = playedAt.getHours() < 12 ? "上午" : playedAt.getHours() < 18 ? "下午" : "晚上";
      return `今天${period}看到这里`;
    }
    return `${Math.floor(elapsed / hour)} 小时前看到这里`;
  }
  if (daysAgo === 1) return "昨天看到这里，接着来";
  if (daysAgo > 1 && daysAgo < 7) return `${daysAgo} 天前看到这里`;
  return "上次看到这里，回来接着看";
}

export function MediaRail({ title, items, landscape = false, linkTo }: { title: string; items: MediaItem[]; landscape?: boolean; linkTo?: string }) {
  if (!items.length) return null;
  return (
    <section className="lux-section" aria-label={title}>
      <div className="lux-section-heading"><h2>{linkTo ? <Link className="lux-section-title-link" to={linkTo}>{title}</Link> : title}</h2><span>{items.length} 项</span></div>
      <HorizontalScrollRail className="lux-home-rail" ariaLabel={title}>
        <div className={landscape ? "lux-media-rail lux-media-rail-landscape" : "lux-media-rail"}>
          {items.map((item) => <MediaCard item={item} landscape={landscape} key={item.id} />)}
        </div>
      </HorizontalScrollRail>
    </section>
  );
}

type LibraryAction = "reidentify" | "scan";

type LibraryMenuPosition = {
  left: number;
  top: number;
};

export function positionLibraryActionMenu(
  point: { left: number; top: number },
  menu: { width: number; height: number },
  viewport: { width: number; height: number },
): LibraryMenuPosition {
  const edge = 12;
  const gap = 8;
  const left = Math.min(
    Math.max(edge, point.left),
    Math.max(edge, viewport.width - menu.width - edge),
  );
  const below = point.top + gap;
  const above = point.top - menu.height - gap;
  const top = below + menu.height + edge <= viewport.height || above < edge
    ? Math.min(below, Math.max(edge, viewport.height - menu.height - edge))
    : Math.max(edge, above);
  return { left, top };
}

export function LibraryCard({ library, coverItems, onPrefetch }: { library: Library; coverItems?: MediaItem[]; onPrefetch?: () => void }) {
  const isSystemCollection = library.id === GLOBAL_COLLECTION_LIBRARY_ID;
  const cardRef = useRef<HTMLElement>(null);
  const menuTriggerRef = useRef<HTMLButtonElement>(null);
  const [menuOpen, setMenuOpen] = useState(false);
  const [menuAnchor, setMenuAnchor] = useState<LibraryMenuPosition>({ left: 12, top: 12 });
  const [pendingAction, setPendingAction] = useState<LibraryAction>();
  const [actionNotice, setActionNotice] = useState<string>();
  const [actionError, setActionError] = useState<string>();

  const { menuRef, position: menuPosition } = usePopoverPosition({
    open: menuOpen,
    onClose: () => setMenuOpen(false),
    isInside: (target) => Boolean(cardRef.current?.contains(target)),
    closeOnResize: true,
    closeOnContextMenu: true,
    fallbackMenuSize: { width: 240, height: 160 },
    getPosition: (menu, viewport) => positionLibraryActionMenu(menuAnchor, menu, viewport),
  });

  function openMenu(point: LibraryMenuPosition) {
    setActionError(undefined);
    setActionNotice(undefined);
    setMenuAnchor(point);
    setMenuOpen(true);
  }

  function handleContextMenu(event: MouseEvent<HTMLElement>) {
    if (isSystemCollection) return;
    event.preventDefault();
    openMenu({ left: event.clientX, top: event.clientY });
  }

  function handleTriggerClick(event: MouseEvent<HTMLButtonElement>) {
    event.preventDefault();
    event.stopPropagation();
    if (menuOpen) {
      setMenuOpen(false);
      return;
    }
    const rect = menuTriggerRef.current?.getBoundingClientRect();
    openMenu({ left: rect?.right ?? event.clientX, top: rect?.bottom ?? event.clientY });
  }

  async function runAction(action: LibraryAction) {
    setPendingAction(action);
    setActionError(undefined);
    setActionNotice(undefined);
    setMenuOpen(false);
    try {
      if (action === "reidentify") {
        const task = await api.startLibraryMetadataReidentify(library.id);
        setActionNotice(`整库元数据匹配任务已提交 · ${task.totalCount} 项，单个后台任务处理中`);
      } else {
        await api.startAdminScan(library.id);
        setActionNotice("扫描媒体库文件任务已提交");
      }
    } catch (cause) {
      setActionError(cause instanceof Error ? cause.message : "媒体库操作提交失败，请重试。");
    } finally {
      setPendingAction(undefined);
    }
  }

  return (
    <article ref={cardRef} className="lux-library-card" onContextMenu={handleContextMenu}>
      <div className="lux-library-card-cover-shell">
        <Link className="lux-library-card-cover-link" to={`/libraries/${library.id}`} onMouseEnter={isSystemCollection ? undefined : onPrefetch} onFocus={isSystemCollection ? undefined : onPrefetch}>
          <span className="lux-library-card-cover">
            {library.coverImageUrl ? <img className="lux-library-cover lux-library-cover-full" src={library.coverImageUrl} alt="" loading="lazy" decoding="async" /> : isSystemCollection ? <GlobalCollectionLibraryCover items={coverItems ?? []} /> : <span className="lux-library-icon" aria-hidden="true">{library.kind === "SERIES" ? "▣" : "◈"}</span>}
          </span>
        </Link>
        {!isSystemCollection ? <button
          ref={menuTriggerRef}
          className="lux-library-card-menu-trigger"
          type="button"
          aria-label={`打开 ${library.name} 操作菜单`}
          aria-haspopup="menu"
          aria-expanded={menuOpen}
          onClick={handleTriggerClick}
        >
          <MoreHorizontal size={16} aria-hidden="true" />
        </button> : null}
      </div>
      <Link className="lux-library-card-copy-link" to={`/libraries/${library.id}`} onMouseEnter={isSystemCollection ? undefined : onPrefetch} onFocus={isSystemCollection ? undefined : onPrefetch}>
        <span className="lux-library-card-copy"><strong>{library.name}</strong></span>
      </Link>
      {actionNotice ? <p className="lux-library-card-action-feedback" role="status">{actionNotice}</p> : null}
      {actionError ? <p className="lux-library-card-action-feedback is-error" role="alert">{actionError}</p> : null}
      {pendingAction ? <span className="lux-visually-hidden" role="status">{pendingAction === "reidentify" ? "正在提交整库元数据匹配任务" : "正在提交扫描任务"}</span> : null}
      {menuOpen && !isSystemCollection ? createPortal(
        <div
          ref={menuRef}
          className="lux-library-action-menu is-context"
          role="menu"
          aria-label={`${library.name} 操作`}
          onContextMenu={(event) => event.preventDefault()}
          style={{ left: menuPosition.left, top: menuPosition.top, right: "auto", position: "fixed" }}
        >
          <div className="lux-library-action-menu-heading">
            {library.coverImageUrl ? <img src={library.coverImageUrl} alt="" /> : <span aria-hidden="true"><Database size={16} /></span>}
            <strong>{library.name}</strong>
          </div>
          <button type="button" role="menuitem" data-library-action="reidentify" disabled={Boolean(pendingAction)} onClick={() => void runAction("reidentify")}>
            <ScanSearch size={18} aria-hidden="true" />
            <span>{pendingAction === "reidentify" ? "提交元数据匹配中…" : "元数据匹配"}</span>
          </button>
          <button type="button" role="menuitem" data-library-action="scan" disabled={Boolean(pendingAction)} onClick={() => void runAction("scan")}>
            <ScanLine size={18} aria-hidden="true" />
            <span>{pendingAction === "scan" ? "提交扫描中…" : "扫描媒体库文件"}</span>
          </button>
        </div>,
        document.body,
      ) : null}
    </article>
  );
}

function GlobalCollectionLibraryCover({ items }: { items: MediaItem[] }) {
  const { enabled } = useImageVariants();
  const posterItems = [
    ...items.filter((item) => item.posterItemId === item.id),
    ...items.filter((item) => item.posterItemId !== item.id),
  ];
  const posters = posterItems
    .map((item) => ({ id: item.id, src: imageUrl(item, "poster", enabled ? 360 : undefined) }))
    .filter((poster): poster is { id: string; src: string } => Boolean(poster.src))
    .slice(0, 5);

  return (
    <span className="lux-global-collection-library-cover" aria-hidden="true">
      <span className="lux-global-collection-library-cover-title">
        <strong>合集</strong>
        <span>Collections</span>
      </span>
      <span className="lux-global-collection-library-cover-posters">
        {posters.length ? posters.map((poster, index) => (
          <img
            key={poster.id}
            className={`lux-global-collection-library-poster is-${index}`}
            src={poster.src}
            alt=""
            loading="lazy"
            decoding="async"
          />
        )) : Array.from({ length: 5 }, (_, index) => (
          <span key={index} className={`lux-global-collection-library-poster is-${index} is-fallback`} />
        ))}
      </span>
    </span>
  );
}
