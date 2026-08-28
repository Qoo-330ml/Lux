import { LoaderCircle, Trash2, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { MediaItem } from "../../lib/api/types";
import { mediaTitle } from "../home/media";
import "./MediaDeleteDialog.css";

type MediaDeleteDialogProps = {
  item: MediaItem;
  onClose: () => void;
  onConfirm: () => Promise<void>;
  onDeleted?: () => void;
};

export function MediaDeleteDialog({ item, onClose, onConfirm, onDeleted }: MediaDeleteDialogProps) {
  const closeRef = useRef<HTMLButtonElement>(null);
  const [deleting, setDeleting] = useState(false);
  const [error, setError] = useState<string>();
  const isSeries = item.itemType === "SERIES";

  useEffect(() => {
    closeRef.current?.focus();
    const previousOverflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    const closeOnEscape = (event: KeyboardEvent) => {
      if (!deleting && event.key === "Escape") onClose();
    };
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("keydown", closeOnEscape);
      document.body.style.overflow = previousOverflow;
    };
  }, [deleting, onClose]);

  async function confirm() {
    setDeleting(true);
    setError(undefined);
    try {
      await onConfirm();
      onDeleted?.();
      onClose();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "删除失败，请重试。");
    } finally {
      setDeleting(false);
    }
  }

  return (
    <div className="lux-media-editor-backdrop" role="presentation" onMouseDown={(event) => { if (!deleting && event.target === event.currentTarget) onClose(); }}>
      <section className="lux-media-editor lux-delete-dialog" role="alertdialog" aria-modal="true" aria-labelledby="lux-delete-title" aria-describedby="lux-delete-description">
        <header className="lux-media-editor-header">
          <div>
            <h2 id="lux-delete-title">删除媒体</h2>
          </div>
          <button ref={closeRef} className="lux-media-editor-close" type="button" aria-label="关闭删除确认" disabled={deleting} onClick={onClose}><X size={18} /></button>
        </header>
        <div className="lux-delete-dialog-body">
          <div className="lux-delete-dialog-icon" aria-hidden="true"><Trash2 size={26} /></div>
          <p id="lux-delete-description">
            {isSeries ? `确定要删除“${mediaTitle(item)}”整部剧及所有分集吗？` : `确定要删除“${mediaTitle(item)}”的当前视频版本吗？`}
          </p>
          <small>{isSeries ? "整部剧下所有季度和分集的视频文件，以及同名的字幕、NFO 和图片旁车文件都会被删除。这个操作无法撤销。" : "视频文件以及同名的字幕、NFO 和图片旁车文件都会被删除。这个操作无法撤销。"}</small>
          {error ? <p className="lux-editor-error" role="alert">{error}</p> : null}
          <div className="lux-delete-dialog-actions">
            <button className="lux-button lux-button-secondary" type="button" disabled={deleting} onClick={onClose}>取消</button>
            <button className="lux-button lux-button-danger" data-action="delete-confirm" type="button" disabled={deleting} onClick={() => void confirm()}>
              {deleting ? <LoaderCircle className="lux-spin" size={16} /> : <Trash2 size={16} />}
              {deleting ? "删除中…" : "确认删除"}
            </button>
          </div>
        </div>
      </section>
    </div>
  );
}
