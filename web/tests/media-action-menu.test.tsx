// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MediaActionMenu } from "../src/features/media/MediaActionMenu";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("MediaActionMenu", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
  });

  it("shows only the first four requested media actions and keeps the selected source in download links", async () => {
    const onEditMetadata = vi.fn();
    const onEditImages = vi.fn();
    const onIdentify = vi.fn();
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter>
          <MediaActionMenu
            item={{
              id: "item-1",
              title: "二毛",
              productionYear: 2019,
              imageTags: { poster: "poster-tag" },
              mediaSources: [
                { id: "source-4k", qualityLabel: "2160p", isDefault: true },
              ],
            }}
            onEditMetadata={onEditMetadata}
            onEditImages={onEditImages}
            onIdentify={onIdentify}
          />
        </MemoryRouter>,
      );
    });

    await act(async () => {
      container.querySelector<HTMLButtonElement>(".lux-media-actions-trigger")?.click();
    });

    const actions = [...container.querySelectorAll<HTMLElement>("[role=menuitem]")]
      .map((element) => element.textContent?.replace(/\s+/g, " ").trim());
    expect(actions).toEqual(["下载", "下载到…", "编辑元数据", "编辑图像", "识别"]);
    expect(container.querySelector<HTMLAnchorElement>("[data-action=download]")?.getAttribute("href"))
      .toBe("/api/v1/items/item-1/download?sourceId=source-4k");

    await act(async () => {
      container.querySelector<HTMLElement>("[data-action=edit-metadata]")?.click();
      container.querySelector<HTMLElement>("[data-action=edit-images]")?.click();
    });
    expect(onEditMetadata).toHaveBeenCalledOnce();
    expect(onEditImages).toHaveBeenCalledOnce();

    await act(async () => container.querySelector<HTMLButtonElement>(".lux-media-actions-trigger")?.click());
    await act(async () => container.querySelector<HTMLElement>("[data-action=identify]")?.click());
    expect(onIdentify).toHaveBeenCalledOnce();
  });

  it("closes when Escape is pressed", async () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter>
          <MediaActionMenu item={{ id: "item-1", title: "二毛" }} onEditMetadata={() => undefined} onEditImages={() => undefined} />
        </MemoryRouter>,
      );
    });
    await act(async () => container.querySelector<HTMLButtonElement>(".lux-media-actions-trigger")?.click());
    expect(container.querySelector("[role=menu]")).not.toBeNull();

    await act(async () => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    });
    expect(container.querySelector("[role=menu]")).toBeNull();
  });
});
