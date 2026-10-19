// @vitest-environment jsdom
import { act, memo } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, expect, it, vi } from "vitest";
import { MediaCard } from "../src/features/home/media";
import { api } from "../src/lib/api/client";
import type { MediaItem } from "../src/lib/api/types";

const overlayRenders = vi.hoisted(() => ({ count: 0 }));
vi.mock("../src/features/home/media-card-overlay", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../src/features/home/media-card-overlay")>();
  return { MediaCardOverlay: memo((props: Parameters<typeof actual.MediaCardOverlay>[0]) => {
    overlayRenders.count += 1;
    return <actual.MediaCardOverlay {...props} />;
  }) };
});

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: Root | undefined;
let container: HTMLDivElement;
afterEach(() => { act(() => root?.unmount()); container?.remove(); overlayRenders.count = 0; vi.restoreAllMocks(); });

function mount() {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
}

it("skips unchanged cards on parent updates and still reflects a new item snapshot", () => {
  mount();
  let visualReads = 0;
  const item: MediaItem = { id: "movie", title: "旧标题", itemType: "MOVIE", get productionYear() { visualReads++; return 2026; } };
  const render = (tick: number, media = item) => act(() => root?.render(
    <MemoryRouter><span>{tick}</span><MediaCard item={media} /></MemoryRouter>,
  ));
  render(0);
  const initialReads = visualReads;
  render(1);
  expect(visualReads - initialReads).toBe(0);
  render(2, { ...item, title: "新标题", imageTags: { poster: "v2" }, rating: 9.2 });
  expect(container.querySelector(".lux-media-copy strong")?.textContent).toBe("新标题");
  expect(container.querySelector("img")?.getAttribute("src")).toContain("tag=v2");
  expect(container.querySelector(".lux-card-rating")?.textContent).toBe("9.2");
});

it("keeps the visual subtree unchanged when an action notice or error changes", async () => {
  mount();
  let visualReads = 0;
  const item: MediaItem = { id: "movie", title: "电影", itemType: "MOVIE", get rating() { visualReads++; return 8.4; } };
  const refresh = vi.spyOn(api, "startItemMetadataRefresh").mockResolvedValue({ jobId: "refresh" } as never);
  await act(async () => root?.render(<MemoryRouter><MediaCard item={item} /></MemoryRouter>));
  const initialReads = visualReads;
  const initialOverlayRenders = overlayRenders.count;
  async function triggerRefresh() {
    await act(async () => container.querySelector<HTMLButtonElement>(".lux-media-actions-trigger")?.click());
    await act(async () => document.querySelector<HTMLButtonElement>('[data-action="refresh-metadata"]')?.click());
  }
  await triggerRefresh();
  expect(container.querySelector('[role="status"]')?.textContent).toBe("刷新任务已提交");
  expect(visualReads - initialReads).toBe(0);
  expect(overlayRenders.count - initialOverlayRenders).toBe(0);
  refresh.mockRejectedValueOnce(new Error("刷新失败"));
  await triggerRefresh();
  expect(container.querySelector('[role="alert"]')?.textContent).toBe("刷新失败");
  expect(visualReads - initialReads).toBe(0);
  expect(overlayRenders.count - initialOverlayRenders).toBe(0);
});

it("keeps folder and read-only links and suppresses their interactive controls", () => {
  mount();
  act(() => root?.render(<MemoryRouter>
    <MediaCard item={{ id: "folder", title: "旅行", itemType: "FOLDER" }} folderHref="/libraries/videos?folderPath=trips" selectionMode metadataAttention />
    <MediaCard item={{ id: "collection", title: "合集", itemType: "BOX_SET", memberCount: 3 }} readOnly detailHref="/collections/collection" selectionMode />
  </MemoryRouter>));
  expect(container.querySelector('a[aria-label="打开文件夹 旅行"]')?.getAttribute("href")).toBe("/libraries/videos?folderPath=trips");
  expect(container.querySelector('a[aria-label="查看 合集 详情"]')?.getAttribute("href")).toBe("/collections/collection");
  expect(container.querySelector(".lux-media-selection-checkbox")).toBeNull();
  expect(container.querySelector(".lux-media-actions")).toBeNull();
  expect(container.querySelector(".lux-metadata-attention-badge")).toBeNull();
  expect(container.querySelector(".lux-media-episode-count")?.textContent).toBe("3 部");
});

it("preserves landscape artwork, progress, metadata attention, and selection identity", () => {
  mount();
  const selection = vi.fn();
  act(() => root?.render(<MemoryRouter><MediaCard item={{ id: "episode", title: "第一集", itemType: "EPISODE", productionYear: 2026, rating: 8.1, runtimeTicks: 1000, userData: { positionTicks: 250 }, imageTags: { poster: "poster-v1", thumb: "thumb-v2" } }} landscape metadataAttention selectionMode detailSearch="?metadataStatus=pending" onSelectionChange={selection} /></MemoryRouter>));
  expect(container.querySelector(".lux-media-card-landscape")).not.toBeNull();
  expect(container.querySelector("img")?.getAttribute("src")).toBe("/api/v1/items/episode/images/thumb?tag=thumb-v2");
  expect(container.querySelector(".lux-progress span")?.getAttribute("style")).toContain("25%");
  expect(container.querySelector(".lux-metadata-attention-badge")?.textContent).toBe("待确认");
  expect(container.querySelector(".lux-media-copy")?.textContent).toContain("2026 · 单集");
  expect(container.querySelector("a")?.getAttribute("href")).toBe("/items/episode?metadataStatus=pending");
  act(() => container.querySelector<HTMLInputElement>("input")?.click());
  expect(selection).toHaveBeenCalledWith("episode", true);
});
