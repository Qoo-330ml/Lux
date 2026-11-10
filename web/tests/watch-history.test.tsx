// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter } from "react-router-dom";
import { WatchHistoryPage } from "../src/features/history/WatchHistoryPage";
import {
  watchHistoryProgressLabel,
  watchHistorySubtitle,
  watchHistoryTimeLabel,
  watchHistoryTitle,
} from "../src/features/history/watch-history-format";
import { api } from "../src/lib/api/client";
import type { MediaItem } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const episode: MediaItem = {
  id: "episode-1",
  itemType: "EPISODE",
  title: "试播集",
  seriesName: "示例剧集",
  seriesId: "series-1",
  parentIndexNumber: 1,
  indexNumber: 2,
  runtimeTicks: 2_000_000_000,
  userData: {
    positionTicks: 1_000_000_000,
    lastPlayedAt: Math.floor(Date.now() / 1000),
  },
};

const movie: MediaItem = {
  id: "movie-1",
  itemType: "MOVIE",
  title: "已看完的电影",
  productionYear: 2021,
  userData: { isPlayed: true, lastPlayedAt: Math.floor(Date.now() / 1000) - 90_000 },
};

describe("watch history formatting", () => {
  it("describes recent watch times in the words a history list reads best", () => {
    const now = new Date(2026, 2, 5, 21, 0);
    const today = new Date(2026, 2, 5, 20, 14);
    const yesterday = new Date(2026, 2, 4, 18, 5);
    const threeDaysAgo = new Date(2026, 2, 2, 9, 0);
    const lastMonth = new Date(2026, 1, 20, 8, 30);
    const lastYear = new Date(2025, 11, 31, 23, 59);

    expect(watchHistoryTimeLabel(today.getTime() / 1000, now)).toBe("今天 20:14");
    expect(watchHistoryTimeLabel(yesterday.getTime() / 1000, now)).toBe("昨天 18:05");
    expect(watchHistoryTimeLabel(threeDaysAgo.getTime() / 1000, now)).toBe("3 天前");
    expect(watchHistoryTimeLabel(lastMonth.getTime() / 1000, now)).toBe("02-20 08:30");
    expect(watchHistoryTimeLabel(lastYear.getTime() / 1000, now)).toBe("2025-12-31 23:59");
    expect(watchHistoryTimeLabel(null, now)).toBeUndefined();
    expect(watchHistoryTimeLabel(0, now)).toBeUndefined();
    expect(watchHistoryTimeLabel(1.5, now)).toBeUndefined();
  });

  it("keeps episodes grouped under their series and reports watching progress", () => {
    expect(watchHistoryTitle(episode)).toBe("示例剧集");
    expect(watchHistorySubtitle(episode)).toBe("S01E02·试播集");
    expect(watchHistoryProgressLabel(episode)).toBe("已看 50%");
    expect(watchHistoryTitle(movie)).toBe("已看完的电影");
    expect(watchHistorySubtitle(movie)).toBe("2021");
    expect(watchHistoryProgressLabel(movie)).toBe("已看完");
  });
});

describe("WatchHistoryPage", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
    vi.restoreAllMocks();
  });

  async function render() {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <WatchHistoryPage />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await settle();
    return container;
  }

  async function settle() {
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
  }

  beforeEach(() => {
    vi.spyOn(api, "watchHistory").mockResolvedValue({
      items: [episode, movie],
      page: 1,
      pageSize: 24,
      total: 30,
      hasNext: true,
    });
  });

  it("renders the user's history with poster, episode info, watch time and progress", async () => {
    const view = await render();

    expect(view.querySelector("h1")?.textContent).toBe("观看历史");
    expect(view.textContent).toContain("共 30 项");
    const rows = view.querySelectorAll(".lux-history-row");
    expect(rows).toHaveLength(2);
    expect(rows[0]?.querySelector(".lux-history-title")?.textContent).toBe("示例剧集");
    expect(rows[0]?.textContent).toContain("S01E02·试播集");
    expect(rows[0]?.textContent).toMatch(/今天 \d{2}:\d{2}/);
    expect(rows[0]?.textContent).toContain("已看 50%");
    expect(rows[0]?.querySelector<HTMLAnchorElement>(".lux-history-title")?.getAttribute("href")).toBe("/items/episode-1");
    expect(rows[0]?.querySelector<HTMLAnchorElement>(".lux-history-resume")?.getAttribute("href")).toBe("/watch/episode-1");
    expect(rows[0]?.querySelector<HTMLElement>(".lux-history-progress span")?.style.width).toBe("50%");
    expect(rows[1]?.textContent).toContain("已看完");
    expect(rows[1]?.querySelector(".lux-history-resume")?.textContent).toContain("重新播放");
  });

  it("pages through the history on the server", async () => {
    const view = await render();

    const next = Array.from(view.querySelectorAll<HTMLButtonElement>("button")).find((button) => button.textContent === "下一页");
    expect(next?.disabled).toBe(false);
    act(() => next?.click());
    await settle();

    expect(api.watchHistory).toHaveBeenLastCalledWith(2);
  });

  it("shows an empty state when the user has not watched anything yet", async () => {
    vi.mocked(api.watchHistory).mockResolvedValue({ items: [], page: 1, pageSize: 24, total: 0, hasNext: false });
    const view = await render();

    expect(view.textContent).toContain("还没有观看记录。");
    expect(view.querySelector(".lux-history-row")).toBeNull();
    expect(view.querySelector(".lux-history-pagination")).toBeNull();
  });
});
