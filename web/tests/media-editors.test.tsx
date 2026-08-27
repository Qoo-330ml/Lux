// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MediaImageEditor } from "../src/features/media/MediaImageEditor";
import { MediaMetadataEditor } from "../src/features/media/MediaMetadataEditor";
import { MediaSubtitleEditor } from "../src/features/media/MediaSubtitleEditor";
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
    expect(search).toHaveBeenCalledWith("item-1", { imageType: "POSTER", language: "zh-CN", source: "" });
    expect(container.querySelector<HTMLElement>(".lux-image-result")?.dataset.imageType).toBe("POSTER");

    await act(async () => container.querySelectorAll<HTMLButtonElement>(".lux-select-trigger")[0]?.click());
    await act(async () => document.querySelector<HTMLButtonElement>("[role=option][data-value='en-US']")?.click());
    expect(search).toHaveBeenLastCalledWith("item-1", { imageType: "POSTER", language: "en-US", source: "" });
  });

  it("marks the current image container with the selected image type", async () => {
    vi.spyOn(api, "itemImages").mockResolvedValue({ images: [] });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    await act(async () => {
      root.render(<MediaImageEditor item={{ id: "item-1", title: "示例电影" }} onClose={() => undefined} />);
    });
    await act(async () => await Promise.resolve());

    const currentCard = () => container.querySelector<HTMLElement>(".lux-image-current-card");
    expect(currentCard()?.dataset.imageType).toBe("POSTER");

    for (const [label, imageType] of [["徽标", "LOGO"], ["缩略图", "THUMB"], ["横幅图", "BANNER"], ["光盘封面", "DISC"], ["艺术图", "ART"], ["壁纸", "WALLPAPER"]]) {
      await act(async () => {
        const tab = [...container.querySelectorAll<HTMLButtonElement>(".lux-image-type-tabs [role=tab]")]
          .find((button) => button.textContent?.includes(label));
        tab?.click();
      });
      expect(currentCard()?.dataset.imageType).toBe(imageType);
    }
  });

  it("edits an indexed external subtitle without exposing embedded tracks", async () => {
    const update = vi.spyOn(api, "updateItemSubtitle").mockResolvedValue({
      sourceId: "source-1",
      streamIndex: 2,
      title: "简体中文",
      language: "zho",
      isDefault: true,
      isForced: false,
    });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    await act(async () => {
      root.render(<MediaSubtitleEditor item={{
        id: "item-1",
        title: "示例电影",
        mediaSources: [{
          id: "source-1",
          isDefault: true,
          streams: [
            { index: 1, type: "SUBTITLE", language: "eng", isExternal: false },
            { index: 2, type: "SUBTITLE", language: "zho", isExternal: true, isDefault: true, isForced: false },
          ],
        }],
      }} onClose={() => undefined} />);
    });
    await act(async () => await Promise.resolve());

    expect(container.querySelectorAll("[data-subtitle-index]")).toHaveLength(1);
    await act(async () => {
      const title = container.querySelector<HTMLInputElement>("#subtitle-title");
      if (title) {
        const setValue = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
        setValue?.call(title, "简体中文");
        title.dispatchEvent(new Event("input", { bubbles: true }));
        title.dispatchEvent(new Event("change", { bubbles: true }));
      }
      container.querySelector<HTMLFormElement>("#subtitle-editor-form")?.requestSubmit();
    });
    expect(update).toHaveBeenCalledWith("item-1", 2, {
      sourceId: "source-1",
      title: "简体中文",
      language: "zho",
      isDefault: true,
      isForced: false,
    });
  });
});
