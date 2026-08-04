import { Download, Ellipsis, FileDown, Image as ImageIcon, Pencil, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { MediaItem } from "../../lib/api/types";

type MediaActionMenuProps = {
  item: MediaItem;
  onEditMetadata: () => void;
  onEditImages: () => void;
  className?: string;
  sourceId?: string;
};

export function mediaDownloadUrl(item: MediaItem, sourceId?: string) {
  const source = sourceId
    ? item.mediaSources?.find((entry) => entry.id === sourceId)
    : item.mediaSources?.find((entry) => entry.isDefault) ?? item.mediaSources?.[0];
  const query = source ? `?sourceId=${encodeURIComponent(source.id)}` : "";
  return `/api/v1/items/${encodeURIComponent(item.id)}/download${query}`;
}

export function MediaActionMenu({ item, onEditMetadata, onEditImages, className = "", sourceId }: MediaActionMenuProps) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const title = item.title || item.name || "媒体";
  const posterUrl = item.imageTags?.poster
    ? `/api/v1/items/${encodeURIComponent(item.id)}/images/poster`
    : undefined;
  const downloadUrl = mediaDownloadUrl(item, sourceId);

  useEffect(() => {
    if (!open) return undefined;

    const closeOnOutsidePointer = (event: PointerEvent) => {
      if (event.target instanceof Node && !rootRef.current?.contains(event.target)) setOpen(false);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    document.addEventListener("pointerdown", closeOnOutsidePointer);
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("pointerdown", closeOnOutsidePointer);
      document.removeEventListener("keydown", closeOnEscape);
    };
  }, [open]);

  return (
    <div ref={rootRef} className={`lux-media-actions ${className}`.trim()}>
      <button
        className="lux-media-actions-trigger"
        type="button"
        aria-label={`打开 ${title} 操作菜单`}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={(event) => {
          event.preventDefault();
          event.stopPropagation();
          setOpen((value) => !value);
        }}
      >
        {open ? <X size={18} /> : <Ellipsis size={19} />}
      </button>
      {open ? (
        <div className="lux-media-action-menu" role="menu" aria-label={`${title} 操作`}>
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
        </div>
      ) : null}
    </div>
  );
}
