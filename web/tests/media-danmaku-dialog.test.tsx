// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MediaCard } from "../src/features/home/media";
import { MediaDanmakuDialog } from "../src/features/media/MediaDanmakuDialog";
import { api } from "../src/lib/api/client";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("MediaDanmakuDialog", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("searches on open with the series title and queues the selected episode", async () => {
    const search = vi.spyOn(api, "manualDanmakuSearch").mockResolvedValue({
      supported: true,
      items: [{
        animeId: "anime-1",
        animeTitle: "示例动画",
        provider: "dandanplay",
        episodes: [{ episodeId: "episode-1", title: "第 1 话" }],
      }],
    });
    const match = vi.spyOn(api, "startManualDanmakuMatch").mockResolvedValue({
      job: { id: "job-1", libraryId: "library-1", status: "PENDING", totalCount: 1, processedCount: 0 },
    });
    const onClose = vi.fn();
    const onSubmitted = vi.fn();
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    await act(async () => {
      root.render(<MediaDanmakuDialog
        item={{ id: "item-1", itemType: "EPISODE", title: "第 1 集", seriesName: "示例动画" }}
        sourceId="source-1"
        onClose={onClose}
        onSubmitted={onSubmitted}
      />);
    });
    await act(async () => await Promise.resolve());

    expect(search).toHaveBeenCalledWith("item-1", { query: "示例动画", sourceId: "source-1" });
    expect(container.querySelector<HTMLInputElement>("#danmaku-query-item-1")?.value).toBe("示例动画");
    expect(container.textContent).toContain("示例动画");
    expect(container.textContent).toContain("第 1 话");

    const overwrite = container.querySelector<HTMLInputElement>(".lux-danmaku-overwrite input");
    await act(async () => overwrite?.click());
    expect(overwrite?.checked).toBe(true);

    await act(async () => container.querySelector<HTMLButtonElement>("[data-action=danmaku-match]")?.click());

    expect(match).toHaveBeenCalledWith("item-1", {
      sourceId: "source-1",
      provider: "dandanplay",
      animeId: "anime-1",
      episodeId: "episode-1",
      overwrite: true,
    });
    expect(onSubmitted).toHaveBeenCalledOnce();
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("rejects item types the backend cannot match", async () => {
    const search = vi.spyOn(api, "manualDanmakuSearch");
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    await act(async () => {
      root.render(<MediaDanmakuDialog item={{ id: "series-1", itemType: "SERIES", title: "示例剧集" }} onClose={() => undefined} />);
    });
    await act(async () => await Promise.resolve());

    expect(search).not.toHaveBeenCalled();
    expect(container.querySelector("[role=alert]")?.textContent).toContain("仅支持电影和分集");
    expect(container.querySelector<HTMLButtonElement>("[data-action=danmaku-search]")?.disabled).toBe(true);
  });

  it("keeps the dialog open and shows the upstream error when a search fails", async () => {
    vi.spyOn(api, "manualDanmakuSearch").mockRejectedValue(new Error("搜索过于频繁，请稍后再试。"));
    const onClose = vi.fn();
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    await act(async () => {
      root.render(<MediaDanmakuDialog item={{ id: "movie-1", itemType: "MOVIE", title: "示例电影" }} onClose={onClose} />);
    });
    await act(async () => await Promise.resolve());

    expect(container.querySelector("[role=dialog]")).not.toBeNull();
    expect(container.querySelector("[role=alert]")?.textContent).toContain("搜索过于频繁");
    expect(onClose).not.toHaveBeenCalled();
  });

  it("opens the card operation menu on right-click and starts the single-item scrape", async () => {
    const search = vi.spyOn(api, "manualDanmakuSearch").mockResolvedValue({ supported: true, items: [] });
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <MediaCard item={{ id: "movie-1", itemType: "MOVIE", title: "示例电影" }} />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });

    const contextMenuEvent = new MouseEvent("contextmenu", {
      bubbles: true,
      cancelable: true,
      clientX: 120,
      clientY: 90,
    });
    await act(async () => container.querySelector<HTMLElement>(".lux-media-card")?.dispatchEvent(contextMenuEvent));

    expect(contextMenuEvent.defaultPrevented).toBe(true);
    expect(document.body.querySelector("[role=menu]")?.textContent).toContain("刮削弹幕");

    await act(async () => document.body.querySelector<HTMLElement>("[data-action=scrape-danmaku]")?.click());
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });

    expect(search).toHaveBeenCalledWith("movie-1", { query: "示例电影", sourceId: undefined });
    expect(container.querySelector("[role=dialog]")).not.toBeNull();
  });
});
