// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, expect, it, vi } from "vitest";
import { MediaCard } from "../src/features/home/media";
import { api } from "../src/lib/api/client";

const loaded = vi.hoisted(() => [] as string[]);
vi.mock("../src/features/media/MediaMetadataEditor", () => {
  loaded.push("metadata");
  return { MediaMetadataEditor: ({ onClose }: { onClose: () => void }) => <button role="dialog" onClick={onClose}>metadata</button> };
});
vi.mock("../src/features/media/MediaImageEditor", () => {
  loaded.push("images");
  return { MediaImageEditor: ({ onClose }: { onClose: () => void }) => <button role="dialog" onClick={onClose}>images</button> };
});
vi.mock("../src/features/media/MediaSubtitleEditor", () => {
  loaded.push("subtitles");
  return { MediaSubtitleEditor: ({ onClose }: { onClose: () => void }) => <button role="dialog" onClick={onClose}>subtitles</button> };
});
vi.mock("../src/features/media/MediaIdentifier", () => {
  loaded.push("identify");
  return { MediaIdentifier: ({ onClose }: { onClose: () => void }) => <button role="dialog" onClick={onClose}>identify</button> };
});
vi.mock("../src/features/media/MediaDeleteDialog", () => {
  loaded.push("delete");
  return { MediaDeleteDialog: ({ onConfirm, onDeleted, onClose }: { onConfirm: () => Promise<void>; onDeleted: () => void; onClose: () => void }) => (
    <div role="dialog"><button onClick={onClose}>cancel</button><button onClick={async () => { await onConfirm(); onDeleted(); onClose(); }}>confirm</button></div>
  ) };
});

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: Root | undefined;
let container: HTMLDivElement;
afterEach(() => { act(() => root?.unmount()); container?.remove(); vi.restoreAllMocks(); });

it("loads only the requested editor, unmounts on close, and deletes the default source", async () => {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  const deletion = vi.spyOn(api, "deleteItem").mockResolvedValue(undefined);
  await act(async () => root?.render(<MemoryRouter><MediaCard item={{
    id: "movie", title: "示例电影", itemType: "MOVIE",
    mediaSources: [{ id: "other", name: "other" }, { id: "default", name: "default", isDefault: true }],
  }} /></MemoryRouter>));
  expect(loaded).toEqual([]);
  expect(container.querySelector('[role="dialog"]')).toBeNull();

  for (const [action, editor] of [["edit-metadata", "metadata"], ["edit-images", "images"], ["edit-subtitles", "subtitles"], ["identify", "identify"], ["delete", "delete"]]) {
    const previouslyLoaded = [...loaded];
    await act(async () => container.querySelector<HTMLButtonElement>(".lux-media-actions-trigger")?.click());
    await act(async () => document.querySelector<HTMLButtonElement>(`[data-action="${action}"]`)?.click());
    await act(async () => { await vi.waitFor(() => expect(container.querySelector('[role="dialog"]')).not.toBeNull()); });
    expect(loaded).toEqual([...previouslyLoaded, editor]);
    if (editor !== "delete") {
      expect(container.querySelector('[role="dialog"]')?.textContent).toBe(editor);
      await act(async () => container.querySelector<HTMLButtonElement>('[role="dialog"]')?.click());
      expect(container.querySelector('[role="dialog"]')).toBeNull();
    } else {
      await act(async () => container.querySelector<HTMLButtonElement>('[role="dialog"] button:last-child')?.click());
      expect(deletion).toHaveBeenCalledWith("movie", "default");
      expect(container.querySelector(".lux-media-card")).toBeNull();
    }
  }
});
