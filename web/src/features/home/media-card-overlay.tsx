import { memo, type ReactNode } from "react";
import type { MediaItem } from "../../lib/api/types";
import { MediaActionMenu } from "../media/MediaActionMenu";
import type { MediaCardEditor } from "./media-card-editors";
import { mediaTitle } from "./media-display";

type MediaCardOverlayProps = {
  item: MediaItem;
  selectionMode: boolean;
  selected: boolean;
  readOnly: boolean;
  children: ReactNode;
  onSelectionChange?: (itemId: string, selected: boolean) => void;
  onEditorChange: (editor: MediaCardEditor) => void;
  onDelete: () => void;
  onRefreshMetadata: () => void;
  onScanFolder: () => void;
  onLockMetadata: () => void;
  onUnlockMetadata: () => void;
};

export const MediaCardOverlay = memo(function MediaCardOverlay({ item, selectionMode, selected, readOnly, children, onSelectionChange, onEditorChange, ...actions }: MediaCardOverlayProps) {
  const canInteract = !readOnly && item.itemType !== "FOLDER";
  return (
    <div className="lux-media-art-shell">
      {selectionMode && canInteract ? (
        <label className="lux-media-selection-control">
          <input className="lux-media-selection-checkbox" type="checkbox" aria-label={`选择 ${mediaTitle(item)}`} checked={selected} onChange={(event) => onSelectionChange?.(item.id, event.target.checked)} onClick={(event) => event.stopPropagation()} />
          <span aria-hidden="true" />
        </label>
      ) : null}
      {children}
      {canInteract ? <MediaActionMenu item={item} onEditMetadata={() => onEditorChange("metadata")} onEditImages={() => onEditorChange("images")} onEditSubtitles={() => onEditorChange("subtitles")} onIdentify={() => onEditorChange("identify")} {...actions} /> : null}
    </div>
  );
});
