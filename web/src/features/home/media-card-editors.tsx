import { lazy, Suspense } from "react";
import { api } from "../../lib/api/client";
import type { MediaItem } from "../../lib/api/types";

const MetadataEditor = lazy(() => import("../media/MediaMetadataEditor").then((module) => ({ default: module.MediaMetadataEditor })));
const ImageEditor = lazy(() => import("../media/MediaImageEditor").then((module) => ({ default: module.MediaImageEditor })));
const SubtitleEditor = lazy(() => import("../media/MediaSubtitleEditor").then((module) => ({ default: module.MediaSubtitleEditor })));
const DanmakuDialog = lazy(() => import("../media/MediaDanmakuDialog").then((module) => ({ default: module.MediaDanmakuDialog })));
const Identifier = lazy(() => import("../media/MediaIdentifier").then((module) => ({ default: module.MediaIdentifier })));
const DeleteDialog = lazy(() => import("../media/MediaDeleteDialog").then((module) => ({ default: module.MediaDeleteDialog })));

export type MediaCardEditor = "metadata" | "images" | "subtitles" | "danmaku" | "identify";

type MediaCardEditorsProps = {
  item: MediaItem;
  editor?: MediaCardEditor;
  deleteOpen: boolean;
  onClose: () => void;
  onDanmakuSubmitted?: () => void;
  onDeleteClose: () => void;
  onDeleted: () => void;
};

export function MediaCardEditors({ item, editor, deleteOpen, onClose, onDanmakuSubmitted, onDeleteClose, onDeleted }: MediaCardEditorsProps) {
  if (!editor && !deleteOpen) return null;
  return (
    <Suspense fallback={null}>
      {editor === "metadata" ? <MetadataEditor item={item} onClose={onClose} /> : null}
      {editor === "images" ? <ImageEditor item={item} onClose={onClose} /> : null}
      {editor === "subtitles" ? <SubtitleEditor item={item} onClose={onClose} /> : null}
      {editor === "danmaku" ? <DanmakuDialog item={item} onClose={onClose} onSubmitted={onDanmakuSubmitted} /> : null}
      {editor === "identify" ? <Identifier item={item} onClose={onClose} /> : null}
      {deleteOpen ? <DeleteDialog item={item} onClose={onDeleteClose} onConfirm={() => api.deleteItem(item.id, item.mediaSources?.find((source) => source.isDefault)?.id)} onDeleted={onDeleted} /> : null}
    </Suspense>
  );
}
