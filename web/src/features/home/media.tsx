import { Database, MoreHorizontal, Play, ScanLine, ScanSearch, Star } from "lucide-react";
import { createPortal } from "react-dom";
import { Link } from "react-router-dom";
import { useEffect, useLayoutEffect, useRef, useState, type MouseEvent } from "react";
import { HorizontalScrollRail } from "../../components/layout/HorizontalScrollRail";
import { api } from "../../lib/api/client";
import type { Library, MediaItem } from "../../lib/api/types";
import { MediaActionMenu } from "../media/MediaActionMenu";
import { MediaImageEditor } from "../media/MediaImageEditor";
import { MediaIdentifier } from "../media/MediaIdentifier";
import { MediaMetadataEditor } from "../media/MediaMetadataEditor";
import { MediaSubtitleEditor } from "../media/MediaSubtitleEditor";
import { MediaDeleteDialog } from "../media/MediaDeleteDialog";

export function mediaTitle(item: MediaItem) {
  return item.title || item.name || "未命名媒体";
}

export function episodeTitle(
  item: MediaItem,
  seasonNumber?: number | null,
  fallbackEpisodeNumber?: number,
) {
  const season = paddedEpisodeNumber(seasonNumber ?? item.parentIndexNumber);
  const episode = paddedEpisodeNumber(item.indexNumber ?? fallbackEpisodeNumber);
  if (item.itemType !== "EPISODE" || season == null || episode == null) {
    return mediaTitle(item);
  }
  return `S${season}E${episode} · ${mediaTitle(item)}`;
}

function paddedEpisodeNumber(value?: number | null) {
  return value != null && Number.isInteger(value) && value >= 0
    ? String(value).padStart(2, "0")
    : undefined;
}

export function mediaTypeLabel(itemType?: string | null) {
  switch (itemType) {
    case "MOVIE": return "电影";
    case "SERIES": return "剧集";
    case "SEASON": return "季度";
    case "EPISODE": return "单集";
    case "BOX_SET": return "合集";
    default: return "媒体";
  }
}

export function imageUrl(item: MediaItem, type: "poster" | "fanart" | "thumb" | "logo" = "poster") {
  const resolvedType = item.imageTags?.[type]
    ? type
    : type === "fanart" && item.imageTags?.thumb
      ? "thumb"
      : undefined;
  if (!resolvedType) return undefined;
  const tag = item.imageTags?.[resolvedType];
  const path = `/api/v1/items/${encodeURIComponent(item.id)}/images/${resolvedType}`;
  return tag ? `${path}?tag=${encodeURIComponent(tag)}` : path;
}

export function runtimeLabel(ticks?: number | null) {
  if (!ticks) return undefined;
  const minutes = Math.round(ticks / 10_000_000 / 60);
  if (!minutes) return undefined;
  const hours = Math.floor(minutes / 60);
  return hours ? `${hours}h ${minutes % 60}m` : `${minutes}m`;
}

export function playbackPositionTicks(item: MediaItem) {
  return item.userData?.positionTicks ?? item.userData?.playbackPositionTicks ?? 0;
}

function runtimeTicks(item: MediaItem) {
  return item.runtimeTicks
    ?? item.mediaSources?.find((source) => source.isDefault)?.durationTicks
    ?? item.mediaSources?.[0]?.durationTicks
    ?? 0;
}

export function playbackProgress(item: MediaItem) {
  const duration = runtimeTicks(item);
  const position = playbackPositionTicks(item);
  return duration > 0 ? Math.min(100, Math.round((position / duration) * 100)) : 0;
}

export function remainingRuntimeLabel(item: MediaItem) {
  const duration = runtimeTicks(item);
  const remaining = duration - playbackPositionTicks(item);
  return remaining > 0 ? runtimeLabel(remaining) : undefined;
}

export function EpisodeCount({ item }: { item: MediaItem }) {
  if (item.itemType !== "SERIES" && item.itemType !== "SEASON") return null;
  if (item.episodeCount == null || !Number.isFinite(item.episodeCount) || item.episodeCount < 0) return null;
  const label = `${item.episodeCount} 集`;
  return <span className="lux-media-episode-count" aria-label={label} title={label}>{label}</span>;
}

export function MediaCard({ item, landscape = false, compactRating = false, metadataAttention = false, detailSearch, selectionMode = false, selected = false, onSelectionChange }: { item: MediaItem; landscape?: boolean; compactRating?: boolean; metadataAttention?: boolean; detailSearch?: string; selectionMode?: boolean; selected?: boolean; onSelectionChange?: (selected: boolean) => void }) {
  const image = imageUrl(item, landscape ? "fanart" : "poster") ?? imageUrl(item);
  const progress = playbackProgress(item);
  const [editor, setEditor] = useState<"metadata" | "images" | "subtitles" | "identify">();
  const [actionError, setActionError] = useState<string>();
  const [actionNotice, setActionNotice] = useState<string>();
  const [deleteOpen, setDeleteOpen] = useState(false);
  const [deleted, setDeleted] = useState(false);

  async function setMetadataLock(locked: boolean) {
    setActionError(undefined);
    setActionNotice(undefined);
    try {
      await api.setItemMetadataLock(item.id, locked);
    } catch (cause) {
      setActionError(cause instanceof Error ? cause.message : "元数据锁定状态更新失败，请重试。");
    }
  }

  async function refreshMetadata() {
    setActionError(undefined);
    setActionNotice(undefined);
    try {
      await api.startItemMetadataRefresh(item.id);
      setActionNotice("刷新任务已提交");
    } catch (cause) {
      setActionError(cause instanceof Error ? cause.message : "元数据刷新任务提交失败，请重试。");
    }
  }

  async function scanLibrary() {
    setActionError(undefined);
    setActionNotice(undefined);
    try {
      await api.startItemFolderScan(item.id);
      setActionNotice("所在文件夹扫描任务已提交");
    } catch (cause) {
      setActionError(cause instanceof Error ? cause.message : "媒体库扫描任务提交失败，请重试。");
    }
  }

  if (deleted) return null;
  const detailHref = `/items/${item.id}${detailSearch ?? ""}`;

  return (
    <>
      <article className={[landscape ? "lux-media-card lux-media-card-landscape" : "lux-media-card", selected ? "is-selected" : ""].filter(Boolean).join(" ")}>
        <div className="lux-media-art-shell">
          {selectionMode ? (
            <label className="lux-media-selection-control">
              <input
                className="lux-media-selection-checkbox"
                type="checkbox"
                aria-label={`选择 ${mediaTitle(item)}`}
                checked={selected}
                onChange={(event) => onSelectionChange?.(event.target.checked)}
                onClick={(event) => event.stopPropagation()}
              />
              <span aria-hidden="true" />
            </label>
          ) : null}
          <Link className="lux-media-card-link" to={detailHref} aria-label={`查看 ${mediaTitle(item)} 详情`}>
            <div className="lux-media-art">
              {image ? <img src={image} alt="" loading="lazy" decoding="async" /> : <div className="lux-media-placeholder">{mediaTitle(item)}</div>}
              <Rating value={item.rating} source={item.ratingSource} compact={compactRating} placement="card" />
              <EpisodeCount item={item} />
              <span className="lux-media-hover-play" aria-hidden="true"><Play size={22} fill="currentColor" /></span>
              {progress > 0 && progress < 90 ? <span className="lux-progress"><span style={{ width: `${progress}%` }} /></span> : null}
            </div>
          </Link>
          <MediaActionMenu item={item} onEditMetadata={() => setEditor("metadata")} onEditImages={() => setEditor("images")} onEditSubtitles={() => setEditor("subtitles")} onDelete={() => setDeleteOpen(true)} onIdentify={() => setEditor("identify")} onRefreshMetadata={() => void refreshMetadata()} onScanFolder={() => void scanLibrary()} onLockMetadata={() => void setMetadataLock(true)} onUnlockMetadata={() => void setMetadataLock(false)} />
        </div>
        <Link className="lux-media-card-link" to={detailHref}>
          <div className="lux-media-copy">
            <strong>{mediaTitle(item)}</strong>
            <span>{[item.productionYear, mediaTypeLabel(item.itemType)].filter(Boolean).join(" · ")}</span>
            {metadataAttention ? <span className="lux-metadata-attention-badge">待确认</span> : null}
          </div>
        </Link>
        {actionNotice ? <p className="lux-muted-copy lux-card-action-error" role="status">{actionNotice}</p> : null}
        {actionError ? <p className="lux-editor-error lux-card-action-error" role="alert">{actionError}</p> : null}
      </article>
      {editor === "metadata" ? <MediaMetadataEditor item={item} onClose={() => setEditor(undefined)} /> : null}
      {editor === "images" ? <MediaImageEditor item={item} onClose={() => setEditor(undefined)} /> : null}
      {editor === "subtitles" ? <MediaSubtitleEditor item={item} onClose={() => setEditor(undefined)} /> : null}
      {deleteOpen ? <MediaDeleteDialog item={item} onClose={() => setDeleteOpen(false)} onConfirm={() => api.deleteItem(item.id, item.mediaSources?.find((source) => source.isDefault)?.id)} onDeleted={() => setDeleted(true)} /> : null}
      {editor === "identify" ? <MediaIdentifier item={item} onClose={() => setEditor(undefined)} /> : null}
    </>
  );
}

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
  const image = imageUrl(item, "fanart") ?? imageUrl(item);
  const progress = playbackProgress(item);
  const remaining = remainingRuntimeLabel(item);
  const subtitle = mediaTypeLabel(item.itemType);
  return (
    <Link className="lux-continue-card" to={`/watch/${item.id}`} aria-label={`继续播放 ${mediaTitle(item)}`}>
      <div className="lux-media-art">
        {image ? <img src={image} alt="" loading="lazy" decoding="async" /> : <div className="lux-media-placeholder">{mediaTitle(item)}</div>}
        <Rating value={item.rating} source={item.ratingSource} compact placement="card" />
        <EpisodeCount item={item} />
        <span className="lux-media-hover-play" aria-hidden="true"><Play size={22} fill="currentColor" /></span>
        {progress > 0 && progress < 100 ? <span className="lux-progress"><span style={{ width: `${progress}%` }} /></span> : null}
      </div>
      <div className="lux-continue-copy">
        <div><strong>{mediaTitle(item)}</strong><small>{subtitle}</small></div>
        {remaining ? <span className="lux-continue-remaining">还剩 {remaining}</span> : null}
      </div>
    </Link>
  );
}

export function MediaRail({ title, items, landscape = false, linkTo }: { title: string; items: MediaItem[]; landscape?: boolean; linkTo?: string }) {
  if (!items.length) return null;
  return (
    <section className="lux-section" aria-label={title}>
      <div className="lux-section-heading"><h2>{linkTo ? <Link className="lux-section-title-link" to={linkTo}>{title}</Link> : title}</h2><span>{items.length} 项</span></div>
      <HorizontalScrollRail className="lux-home-rail" ariaLabel={title}>
        <div className={landscape ? "lux-media-rail lux-media-rail-landscape" : "lux-media-rail"}>
          {items.map((item) => <MediaCard item={item} landscape={landscape} compactRating key={item.id} />)}
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

export function LibraryCard({ library, onPrefetch }: { library: Library; onPrefetch?: () => void }) {
  const cardRef = useRef<HTMLElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const menuTriggerRef = useRef<HTMLButtonElement>(null);
  const [menuOpen, setMenuOpen] = useState(false);
  const [menuPosition, setMenuPosition] = useState<LibraryMenuPosition>({ left: 12, top: 12 });
  const [pendingAction, setPendingAction] = useState<LibraryAction>();
  const [actionNotice, setActionNotice] = useState<string>();
  const [actionError, setActionError] = useState<string>();

  useEffect(() => {
    if (!menuOpen) return undefined;
    const closeOnOutsidePointer = (event: PointerEvent) => {
      if (event.target instanceof Node && !cardRef.current?.contains(event.target) && !menuRef.current?.contains(event.target)) setMenuOpen(false);
    };
    const closeOnOutsideContextMenu = (event: globalThis.MouseEvent) => {
      if (event.target instanceof Node && !cardRef.current?.contains(event.target) && !menuRef.current?.contains(event.target)) setMenuOpen(false);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setMenuOpen(false);
    };
    const closeOnResize = () => setMenuOpen(false);
    document.addEventListener("pointerdown", closeOnOutsidePointer);
    document.addEventListener("contextmenu", closeOnOutsideContextMenu);
    document.addEventListener("keydown", closeOnEscape);
    window.addEventListener("resize", closeOnResize);
    return () => {
      document.removeEventListener("pointerdown", closeOnOutsidePointer);
      document.removeEventListener("contextmenu", closeOnOutsideContextMenu);
      document.removeEventListener("keydown", closeOnEscape);
      window.removeEventListener("resize", closeOnResize);
    };
  }, [menuOpen]);

  useLayoutEffect(() => {
    if (!menuOpen || !menuRef.current) return undefined;
    const menu = menuRef.current;
    const updatePosition = () => {
      const rect = menu.getBoundingClientRect();
      setMenuPosition((point) => positionLibraryActionMenu(
        point,
        { width: rect.width || 240, height: rect.height || 160 },
        { width: window.innerWidth, height: window.innerHeight },
      ));
    };
    updatePosition();
    window.addEventListener("scroll", updatePosition, true);
    return () => window.removeEventListener("scroll", updatePosition, true);
  }, [menuOpen]);

  function openMenu(point: LibraryMenuPosition) {
    setActionError(undefined);
    setActionNotice(undefined);
    setMenuPosition(point);
    setMenuOpen(true);
  }

  function handleContextMenu(event: MouseEvent<HTMLElement>) {
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
      <Link className="lux-library-card-link" to={`/libraries/${library.id}`} onMouseEnter={onPrefetch} onFocus={onPrefetch}>
        <span className="lux-library-card-cover">
          {library.coverImageUrl ? <img className="lux-library-cover lux-library-cover-full" src={library.coverImageUrl} alt="" loading="lazy" decoding="async" /> : <span className="lux-library-icon" aria-hidden="true">{library.kind === "SERIES" ? "▣" : "◈"}</span>}
        </span>
        <span className="lux-library-card-copy"><strong>{library.name}</strong></span>
      </Link>
      <button
        ref={menuTriggerRef}
        className="lux-library-card-menu-trigger"
        type="button"
        aria-label={`打开 ${library.name} 操作菜单`}
        aria-haspopup="menu"
        aria-expanded={menuOpen}
        onClick={handleTriggerClick}
      >
        <MoreHorizontal size={18} aria-hidden="true" />
      </button>
      {actionNotice ? <p className="lux-library-card-action-feedback" role="status">{actionNotice}</p> : null}
      {actionError ? <p className="lux-library-card-action-feedback is-error" role="alert">{actionError}</p> : null}
      {pendingAction ? <span className="lux-visually-hidden" role="status">{pendingAction === "reidentify" ? "正在提交整库元数据匹配任务" : "正在提交扫描任务"}</span> : null}
      {menuOpen ? createPortal(
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

export function Rating({ value, source, compact = false, placement = "default" }: { value?: number | null; source?: string | null; compact?: boolean; placement?: "default" | "card" }) {
  if (value == null || !Number.isFinite(value) || value < 0 || value > 10) return null;
  const score = value.toFixed(1);
  const label = compact ? "评分 " + score : source ? source + " 评分 " + score : "评分 " + score;
  return (
    <span className={["lux-rating", compact ? "is-compact" : "", placement === "card" ? "lux-card-rating" : ""].filter(Boolean).join(" ")} aria-label={label} title={label}>
      {compact ? null : <Star size={14} fill="currentColor" aria-hidden="true" />}
      {compact ? null : source ? <span className="lux-rating-source">{source}</span> : null}
      <strong>{score}</strong>
    </span>
  );
}
