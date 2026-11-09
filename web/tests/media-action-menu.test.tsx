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
    const onEditSubtitles = vi.fn();
    const onIdentify = vi.fn();
    const onLockMetadata = vi.fn();
    const onUnlockMetadata = vi.fn();
    const onRefreshMetadata = vi.fn();
    const onScanFolder = vi.fn();
    const onDelete = vi.fn();
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
            onEditSubtitles={onEditSubtitles}
            onIdentify={onIdentify}
            onLockMetadata={onLockMetadata}
            onUnlockMetadata={onUnlockMetadata}
            onRefreshMetadata={onRefreshMetadata}
            onScanFolder={onScanFolder}
            onDelete={onDelete}
          />
        </MemoryRouter>,
      );
    });

    await act(async () => {
      container.querySelector<HTMLButtonElement>(".lux-media-actions-trigger")?.click();
    });

    const actions = [...document.body.querySelectorAll<HTMLElement>("[role=menuitem]")]
      .map((element) => element.textContent?.replace(/\s+/g, " ").trim());
    expect(actions).toEqual(["下载", "下载到…", "编辑元数据", "编辑图像", "编辑字幕", "删除", "元数据匹配", "刷新元数据", "扫描所在文件夹", "锁定元数据", "解锁元数据"]);
    expect(document.body.querySelector<HTMLAnchorElement>("[data-action=download]")?.getAttribute("href"))
      .toBe("/api/v1/items/item-1/download?sourceId=source-4k");

    await act(async () => {
      document.body.querySelector<HTMLElement>("[data-action=edit-metadata]")?.click();
      document.body.querySelector<HTMLElement>("[data-action=edit-images]")?.click();
    });
    expect(onEditMetadata).toHaveBeenCalledOnce();
    expect(onEditImages).toHaveBeenCalledOnce();

    await act(async () => container.querySelector<HTMLButtonElement>(".lux-media-actions-trigger")?.click());
    await act(async () => document.body.querySelector<HTMLElement>("[data-action=edit-subtitles]")?.click());
    expect(onEditSubtitles).toHaveBeenCalledOnce();

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
    await act(async () => document.body.querySelector<HTMLElement>("[data-action=scan-folder]")?.click());
    expect(onScanFolder).toHaveBeenCalledOnce();

    await act(async () => container.querySelector<HTMLButtonElement>(".lux-media-actions-trigger")?.click());
    await act(async () => document.body.querySelector<HTMLElement>("[data-action=delete]")?.click());
    expect(onDelete).toHaveBeenCalledOnce();
  });

  it("labels the refresh action from the resolved scrape mode", async () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter>
          <MediaActionMenu
            item={{ id: "item-1", title: "二毛" }}
            onEditMetadata={() => undefined}
            onEditImages={() => undefined}
            onRefreshMetadata={() => undefined}
            refreshMode="FULL_REFRESH"
          />
        </MemoryRouter>,
      );
    });

    await act(async () => container.querySelector<HTMLButtonElement>(".lux-media-actions-trigger")?.click());

    const action = document.body.querySelector<HTMLButtonElement>("[data-action=refresh-metadata]");
    expect(action?.textContent?.replace(/\s+/g, " ").trim()).toBe("全量刷新元数据");
    expect(action?.getAttribute("title")).toBe("重新获取并覆盖未锁定的元数据和图片");
  });

  it("labels a fill-missing refresh", async () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter>
          <MediaActionMenu
            item={{ id: "item-1", title: "二毛" }}
            onEditMetadata={() => undefined}
            onEditImages={() => undefined}
            onRefreshMetadata={() => undefined}
            refreshMode="FILL_MISSING"
          />
        </MemoryRouter>,
      );
    });

    await act(async () => container.querySelector<HTMLButtonElement>(".lux-media-actions-trigger")?.click());

    const action = document.body.querySelector<HTMLButtonElement>("[data-action=refresh-metadata]");
    expect(action?.textContent?.replace(/\s+/g, " ").trim()).toBe("补全缺失元数据");
    expect(action?.getAttribute("title")).toBe("只补全缺失的元数据和图片");
  });

  it("keeps the neutral label when no mode is resolved", async () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter>
          <MediaActionMenu
            item={{ id: "item-1", title: "二毛" }}
            onEditMetadata={() => undefined}
            onEditImages={() => undefined}
            onRefreshMetadata={() => undefined}
          />
        </MemoryRouter>,
      );
    });

    await act(async () => container.querySelector<HTMLButtonElement>(".lux-media-actions-trigger")?.click());

    const action = document.body.querySelector<HTMLButtonElement>("[data-action=refresh-metadata]");
    expect(action?.textContent?.replace(/\s+/g, " ").trim()).toBe("刷新元数据");
    expect(action?.getAttribute("title")).toBeNull();
  });

  it("offers deletion for series items", async () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter>
          <MediaActionMenu
            item={{ id: "series-1", title: "示例剧集", itemType: "SERIES" }}
            onEditMetadata={() => undefined}
            onEditImages={() => undefined}
            onDelete={() => undefined}
          />
        </MemoryRouter>,
      );
    });

    await act(async () => container.querySelector<HTMLButtonElement>(".lux-media-actions-trigger")?.click());

    expect(document.body.querySelector("[data-action=delete]")).not.toBeNull();
  });

  it("keeps HomeVideos editable without exposing online metadata matching", async () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter>
          <MediaActionMenu
            item={{ id: "video-1", title: "家庭录像", itemType: "VIDEO" }}
            onEditMetadata={() => undefined}
            onEditImages={() => undefined}
            onIdentify={() => undefined}
            onRefreshMetadata={() => undefined}
          />
        </MemoryRouter>,
      );
    });

    await act(async () => container.querySelector<HTMLButtonElement>(".lux-media-actions-trigger")?.click());

    expect(document.body.querySelector("[data-action=edit-metadata]")).not.toBeNull();
    expect(document.body.querySelector("[data-action=identify]")).toBeNull();
    expect(document.body.querySelector("[data-action=refresh-metadata]")).toBeNull();
  });

  it("does not render actions for folders", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter>
          <MediaActionMenu
            item={{ id: "folder-1", title: "Trips", itemType: "FOLDER" }}
            onEditMetadata={() => undefined}
            onEditImages={() => undefined}
          />
        </MemoryRouter>,
      );
    });

    expect(container.querySelector(".lux-media-actions-trigger")).toBeNull();
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

  it("opens at a context-menu point and offers danmaku scraping for movies", async () => {
    const onScrapeDanmaku = vi.fn();
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter>
          <MediaActionMenu
            item={{ id: "item-1", title: "二毛", itemType: "MOVIE" }}
            contextPoint={{ x: 220, y: 180 }}
            onEditMetadata={() => undefined}
            onEditImages={() => undefined}
            onScrapeDanmaku={onScrapeDanmaku}
          />
        </MemoryRouter>,
      );
    });

    const menu = document.body.querySelector<HTMLElement>("[role=menu]");
    expect(menu).not.toBeNull();
    expect(menu?.textContent).toContain("刮削弹幕");
    expect(menu?.style.left).toBe("220px");
    expect(menu?.style.position).toBe("fixed");

    await act(async () => document.body.querySelector<HTMLElement>("[data-action=scrape-danmaku]")?.click());
    expect(onScrapeDanmaku).toHaveBeenCalledOnce();
    expect(document.body.querySelector("[role=menu]")).toBeNull();
  });

  it("hides danmaku scraping for item types without a matchable source", async () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter>
          <MediaActionMenu
            item={{ id: "series-1", title: "示例剧集", itemType: "SERIES" }}
            onEditMetadata={() => undefined}
            onEditImages={() => undefined}
            onScrapeDanmaku={() => undefined}
          />
        </MemoryRouter>,
      );
    });
    await act(async () => container.querySelector<HTMLButtonElement>(".lux-media-actions-trigger")?.click());

    expect(document.body.querySelector("[role=menu]")?.textContent).not.toContain("刮削弹幕");
    expect(document.body.querySelector("[data-action=scrape-danmaku]")).toBeNull();
  });

  it("keeps the portaled menu open for inside pointer events and closes outside", async () => {
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

    const menu = document.body.querySelector<HTMLElement>("[role=menu]");
    const outside = document.createElement("button");
    document.body.append(outside);
    await act(async () => menu?.querySelector<HTMLElement>("[data-action=edit-metadata]")
      ?.dispatchEvent(new Event("pointerdown", { bubbles: true })));
    expect(document.body.querySelector("[role=menu]")).not.toBeNull();

    await act(async () => outside.dispatchEvent(new Event("pointerdown", { bubbles: true })));

    expect(document.body.querySelector("[role=menu]")).toBeNull();
    outside.remove();
  });
});
