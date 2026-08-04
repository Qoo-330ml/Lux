// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MediaImageEditor } from "../src/features/media/MediaImageEditor";
import { MediaMetadataEditor } from "../src/features/media/MediaMetadataEditor";
import { api } from "../src/lib/api/client";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("media editors", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("edits metadata fields and lets each field be locked independently", async () => {
    vi.spyOn(api, "itemMetadata").mockResolvedValue({
      title: "原始标题",
      originalTitle: "Original",
      overview: "简介",
      productionYear: 2020,
      lockedFields: ["title"],
    });
    const update = vi.spyOn(api, "updateItemMetadata").mockResolvedValue({
      title: "新标题",
      lockedFields: ["title"],
    });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    await act(async () => {
      root.render(<MediaMetadataEditor item={{ id: "item-1", title: "原始标题" }} onClose={() => undefined} />);
    });
    await act(async () => await Promise.resolve());

    const title = container.querySelector<HTMLInputElement>("#metadata-title");
    expect(title?.disabled).toBe(true);
    expect(container.querySelector<HTMLButtonElement>("[aria-label='解锁标题']")).not.toBeNull();

    await act(async () => container.querySelector<HTMLButtonElement>("[aria-label='解锁标题']")?.click());
    expect(container.querySelector<HTMLInputElement>("#metadata-title")?.disabled).toBe(false);
    await act(async () => container.querySelector<HTMLButtonElement>("#metadata-title")?.dispatchEvent(new Event("input", { bubbles: true })));
    await act(async () => container.querySelector<HTMLButtonElement>(".lux-metadata-editor-form button[type='submit']")?.click());

    expect(update).toHaveBeenCalledWith("item-1", expect.objectContaining({ lockedFields: [] }));
  });

  it("shows seven image types and sends the selected language/source filters to search", async () => {
    vi.spyOn(api, "itemImages").mockResolvedValue({ images: [] });
    const search = vi.spyOn(api, "searchItemImages").mockResolvedValue({
      images: [{ id: "poster-1", imageType: "POSTER", imageIndex: 0, source: "TMDB", url: "https://image.tmdb.org/poster.jpg" }],
    });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    await act(async () => {
      root.render(<MediaImageEditor item={{ id: "item-1", title: "示例电影" }} onClose={() => undefined} />);
    });
    await act(async () => await Promise.resolve());

    expect(container.querySelectorAll(".lux-image-type-tabs [role=tab]")).toHaveLength(7);
    await act(async () => container.querySelector<HTMLButtonElement>(".lux-image-editor-toolbar .lux-button")?.click());
    expect(search).toHaveBeenCalledWith("item-1", { imageType: "POSTER", language: "zh-CN", source: "TMDB" });

    await act(async () => container.querySelectorAll<HTMLButtonElement>(".lux-select-trigger")[0]?.click());
    await act(async () => document.querySelector<HTMLButtonElement>("[role=option][data-value='en-US']")?.click());
    expect(search).toHaveBeenLastCalledWith("item-1", { imageType: "POSTER", language: "en-US", source: "TMDB" });
  });
});
