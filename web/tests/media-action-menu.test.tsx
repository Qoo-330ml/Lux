// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MediaActionMenu, positionMediaActionMenu } from "../src/features/media/MediaActionMenu";

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
    const onLockMetadata = vi.fn();
    const onUnlockMetadata = vi.fn();
    const onRefreshMetadata = vi.fn();
    const onScanLibrary = vi.fn();
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
            onLockMetadata={onLockMetadata}
            onUnlockMetadata={onUnlockMetadata}
            onRefreshMetadata={onRefreshMetadata}
            onScanLibrary={onScanLibrary}
          />
        </MemoryRouter>,
      );
    });

    await act(async () => {
      container.querySelector<HTMLButtonElement>(".lux-media-actions-trigger")?.click();
    });

    const actions = [...document.body.querySelectorAll<HTMLElement>("[role=menuitem]")]
      .map((element) => element.textContent?.replace(/\s+/g, " ").trim());
    expect(actions).toEqual(["下载", "下载到…", "编辑元数据", "编辑图像", "识别", "刷新元数据", "扫描媒体库文件", "锁定元数据", "解锁元数据"]);
    expect(document.body.querySelector<HTMLAnchorElement>("[data-action=download]")?.getAttribute("href"))
      .toBe("/api/v1/items/item-1/download?sourceId=source-4k");

    await act(async () => {
      document.body.querySelector<HTMLElement>("[data-action=edit-metadata]")?.click();
      document.body.querySelector<HTMLElement>("[data-action=edit-images]")?.click();
    });
    expect(onEditMetadata).toHaveBeenCalledOnce();
    expect(onEditImages).toHaveBeenCalledOnce();

    await act(async () => container.querySelector<HTMLButtonElement>(".lux-media-actions-trigger")?.click());
    await act(async () => document.body.querySelector<HTMLElement>("[data-action=identify]")?.click());
    expect(onIdentify).toHaveBeenCalledOnce();

    await act(async () => container.querySelector<HTMLButtonElement>(".lux-media-actions-trigger")?.click());
    await act(async () => document.body.querySelector<HTMLElement>("[data-action=lock-metadata]")?.click());
    expect(onLockMetadata).toHaveBeenCalledOnce();

    await act(async () => container.querySelector<HTMLButtonElement>(".lux-media-actions-trigger")?.click());
    await act(async () => document.body.querySelector<HTMLElement>("[data-action=unlock-metadata]")?.click());
    expect(onUnlockMetadata).toHaveBeenCalledOnce();

    await act(async () => container.querySelector<HTMLButtonElement>(".lux-media-actions-trigger")?.click());
    await act(async () => document.body.querySelector<HTMLElement>("[data-action=refresh-metadata]")?.click());
    expect(onRefreshMetadata).toHaveBeenCalledOnce();

    await act(async () => container.querySelector<HTMLButtonElement>(".lux-media-actions-trigger")?.click());
    await act(async () => document.body.querySelector<HTMLElement>("[data-action=scan-library]")?.click());
    expect(onScanLibrary).toHaveBeenCalledOnce();
  });

  it("keeps a menu opened from the first resource inside the viewport", () => {
    const position = positionMediaActionMenu(
      { top: 40, bottom: 74, left: 20, right: 54 },
      { width: 246, height: 360 },
      { width: 464, height: 804 },
    );

    expect(position).toEqual({ left: 16, top: 82 });
  });

  it("renders the open menu outside the resource rail so it cannot be clipped", async () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <div className="lux-media-rail">
          <MediaActionMenu item={{ id: "item-1", title: "二毛" }} onEditMetadata={() => undefined} onEditImages={() => undefined} />
        </div>,
      );
    });

    await act(async () => container.querySelector<HTMLButtonElement>(".lux-media-actions-trigger")?.click());

    expect(document.body.querySelector("[role=menu]")?.parentElement).toBe(document.body);
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
    expect(document.body.querySelector("[role=menu]")).not.toBeNull();

    await act(async () => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    });
    expect(document.body.querySelector("[role=menu]")).toBeNull();
  });
});
