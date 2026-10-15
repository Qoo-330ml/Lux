// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, expect, it, vi } from "vitest";
import { MediaCard } from "../src/features/home/media";
import { api } from "../src/lib/api/client";
import type { MediaItem } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: Root | undefined;
let container: HTMLDivElement;
afterEach(() => { act(() => root?.unmount()); container?.remove(); vi.restoreAllMocks(); });

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
  async function triggerRefresh() {
    await act(async () => container.querySelector<HTMLButtonElement>(".lux-media-actions-trigger")?.click());
    await act(async () => document.querySelector<HTMLButtonElement>('[data-action="refresh-metadata"]')?.click());
  }
  await triggerRefresh();
  expect(container.querySelector('[role="status"]')?.textContent).toBe("刷新任务已提交");
  expect(visualReads - initialReads).toBe(0);
  refresh.mockRejectedValueOnce(new Error("刷新失败"));
  await triggerRefresh();
  expect(container.querySelector('[role="alert"]')?.textContent).toBe("刷新失败");
  expect(visualReads - initialReads).toBe(0);
});
