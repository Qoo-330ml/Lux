import { Download, Ellipsis, FileDown, Image as ImageIcon, Lock, Pencil, RefreshCw, ScanLine, ScanSearch, Subtitles, Trash2, Unlock, X } from "lucide-react";
import { createPortal } from "react-dom";
import { useRef, useState } from "react";
import type { MediaItem } from "../../lib/api/types";
import { usePopoverPosition } from "./usePopoverPosition";

type MediaActionMenuProps = {
  item: MediaItem;
  onEditMetadata: () => void;
  onEditImages: () => void;
  onEditSubtitles?: () => void;
  onDelete?: () => void;
  onIdentify?: () => void;
  onLockMetadata?: () => void;
  onUnlockMetadata?: () => void;
  onRefreshMetadata?: () => void;
  onScanFolder?: () => void;
  className?: string;
  sourceId?: string;
  showLabel?: boolean;
};

type MediaActionRect = Pick<DOMRect, "top" | "bottom" | "left" | "right">;
type MediaActionSize = Pick<DOMRect, "width" | "height">;
type MediaViewport = Pick<DOMRect, "width" | "height">;

const NON_MEDIA_ITEM_TYPES = new Set(["SEASON", "BOX_SET", "FOLDER"]);

export function positionMediaActionMenu(
  trigger: MediaActionRect,
  menu: MediaActionSize,
  viewport: MediaViewport,
) {
  const edge = 16;
  const gap = 8;
  const left = Math.min(
    Math.max(edge, trigger.right - menu.width),
    Math.max(edge, viewport.width - menu.width - edge),
  );
  const below = trigger.bottom + gap;
  const above = trigger.top - menu.height - gap;
  const top = below + menu.height + edge <= viewport.height || above < edge
    ? Math.min(below, Math.max(edge, viewport.height - menu.height - edge))
    : Math.max(edge, above);
  return { left, top };
}

export function mediaDownloadUrl(item: MediaItem, sourceId?: string) {
  const source = sourceId
    ? item.mediaSources?.find((entry) => entry.id === sourceId)
    : item.mediaSources?.find((entry) => entry.isDefault) ?? item.mediaSources?.[0];
  const query = source ? `?sourceId=${encodeURIComponent(source.id)}` : "";
  return `/api/v1/items/${encodeURIComponent(item.id)}/download${query}`;
}

function canDeleteMediaSource(item: MediaItem) {
  return !NON_MEDIA_ITEM_TYPES.has(item.itemType ?? "");
}

export function MediaActionMenu({ item, onEditMetadata, onEditImages, onEditSubtitles, onDelete, onIdentify, onLockMetadata, onUnlockMetadata, onRefreshMetadata, onScanFolder, className = "", sourceId, showLabel = false }: MediaActionMenuProps) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const { menuRef, position: menuPosition } = usePopoverPosition({
    open,
    onClose: () => setOpen(false),
    isInside: (target) => Boolean(rootRef.current?.contains(target)),
    fallbackMenuSize: { width: 246, height: 360 },
    getPosition: (menu, viewport) => {
      const trigger = triggerRef.current?.getBoundingClientRect();
      return trigger
        ? positionMediaActionMenu(trigger, menu, viewport)
        : { left: 16, top: 16 };
    },
  });
  const title = item.title || item.name || "媒体";
  const isFolder = item.itemType === "FOLDER";
  const isVideo = item.itemType === "VIDEO";
  const posterUrl = item.imageTags?.poster
    ? `/api/v1/items/${encodeURIComponent(item.id)}/images/poster`
    : undefined;
  const downloadUrl = mediaDownloadUrl(item, sourceId);

  if (isFolder) return null;

  return (
    <div ref={rootRef} className={`lux-media-actions ${className}`.trim()}>
      <button
        ref={triggerRef}
        className="lux-media-actions-trigger"
        type="button"
        aria-label={showLabel ? "更多" : `打开 ${title} 操作菜单`}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={(event) => {
          event.preventDefault();
          event.stopPropagation();
          setOpen((value) => !value);
        }}
      >
        <span className="lux-media-actions-trigger-icon" aria-hidden="true">{open ? <X size={18} /> : <Ellipsis size={19} />}</span>
        {showLabel ? <span className="lux-media-actions-trigger-label">更多</span> : null}
      </button>
      {open ? createPortal(
        <div ref={menuRef} className="lux-media-action-menu" role="menu" aria-label={`${title} 操作`} style={{ left: menuPosition.left, top: menuPosition.top, position: "fixed" }}>
          <div className="lux-media-action-menu-heading">
            {posterUrl ? <img src={posterUrl} alt="" /> : <span aria-hidden="true">{title.slice(0, 1)}</span>}
            <div>
              <strong>{title}</strong>
              {item.productionYear ? <small>{item.productionYear}</small> : null}
            </div>
          </div>
          <a className="lux-media-action" data-action="download" role="menuitem" href={downloadUrl} download onClick={() => setOpen(false)}>
            <Download size={17} aria-hidden="true" />
            <span>下载</span>
          </a>
          <a className="lux-media-action" data-action="download-to" role="menuitem" href={downloadUrl} download onClick={() => setOpen(false)}>
            <FileDown size={17} aria-hidden="true" />
            <span>下载到…</span>
          </a>
          <button
            className="lux-media-action"
            data-action="edit-metadata"
            type="button"
            role="menuitem"
            onClick={() => {
              setOpen(false);
              onEditMetadata();
            }}
          >
            <Pencil size={17} aria-hidden="true" />
            <span>编辑元数据</span>
          </button>
          <button
            className="lux-media-action"
            data-action="edit-images"
            type="button"
            role="menuitem"
            onClick={() => {
              setOpen(false);
              onEditImages();
            }}
          >
            <ImageIcon size={17} aria-hidden="true" />
            <span>编辑图像</span>
          </button>
          {onEditSubtitles ? (
            <button
              className="lux-media-action"
              data-action="edit-subtitles"
              type="button"
              role="menuitem"
              onClick={() => {
                setOpen(false);
                onEditSubtitles();
              }}
            >
              <Subtitles size={17} aria-hidden="true" />
              <span>编辑字幕</span>
            </button>
          ) : null}
          {onDelete && canDeleteMediaSource(item) ? (
            <button
              className="lux-media-action lux-media-action-danger"
              data-action="delete"
              type="button"
              role="menuitem"
              onClick={() => {
                setOpen(false);
                onDelete();
              }}
            >
              <Trash2 size={17} aria-hidden="true" />
              <span>删除</span>
            </button>
          ) : null}
          {onIdentify && !isVideo ? (
            <button
              className="lux-media-action"
              data-action="identify"
              type="button"
              role="menuitem"
              onClick={() => {
                setOpen(false);
                onIdentify();
              }}
            >
              <ScanSearch size={17} aria-hidden="true" />
              <span>元数据匹配</span>
            </button>
          ) : null}
          {onRefreshMetadata && !isVideo ? (
            <button
              className="lux-media-action"
              data-action="refresh-metadata"
              type="button"
              role="menuitem"
              onClick={() => {
                setOpen(false);
                onRefreshMetadata();
              }}
            >
              <RefreshCw size={17} aria-hidden="true" />
              <span>刷新元数据</span>
            </button>
          ) : null}
          {onScanFolder ? (
            <button
              className="lux-media-action"
              data-action="scan-folder"
              type="button"
              role="menuitem"
              onClick={() => {
                setOpen(false);
                onScanFolder();
              }}
            >
              <ScanLine size={17} aria-hidden="true" />
              <span>扫描所在文件夹</span>
            </button>
          ) : null}
          {onLockMetadata ? (
            <button
              className="lux-media-action"
              data-action="lock-metadata"
              type="button"
              role="menuitem"
              onClick={() => {
                setOpen(false);
                onLockMetadata();
              }}
            >
              <Lock size={17} aria-hidden="true" />
              <span>锁定元数据</span>
            </button>
          ) : null}
          {onUnlockMetadata ? (
            <button
              className="lux-media-action"
              data-action="unlock-metadata"
              type="button"
              role="menuitem"
              onClick={() => {
                setOpen(false);
                onUnlockMetadata();
              }}
            >
              <Unlock size={17} aria-hidden="true" />
              <span>解锁元数据</span>
            </button>
          ) : null}
        </div>,
        document.body,
      ) : null}
    </div>
  );
}
