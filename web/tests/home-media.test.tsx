// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it } from "vitest";
import { ContinueWatchingRail, MediaCard, MediaRail, posterUrlWithFallback, recentPlaybackLabel } from "../src/features/home/media";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("posterUrlWithFallback", () => {
  it("uses the series poster when a season has no poster", () => {
    expect(posterUrlWithFallback(
      { id: "season-1", itemType: "SEASON" },
      { id: "series-1", itemType: "SERIES", imageTags: { poster: "series-poster" } },
    )).toBe("/api/v1/items/series-1/images/poster?tag=series-poster");
  });

  it("keeps a season poster ahead of the series fallback", () => {
    expect(posterUrlWithFallback(
      { id: "season-1", itemType: "SEASON", imageTags: { poster: "season-poster" } },
      { id: "series-1", itemType: "SERIES", imageTags: { poster: "series-poster" } },
    )).toBe("/api/v1/items/season-1/images/poster?tag=season-poster");
  });

  it("does not use a fallback without a series parent", () => {
    expect(posterUrlWithFallback({ id: "season-1", itemType: "SEASON" })).toBeUndefined();
    expect(posterUrlWithFallback(
      { id: "movie-1", itemType: "MOVIE" },
      { id: "series-1", itemType: "SERIES", imageTags: { poster: "series-poster" } },
    )).toBeUndefined();
  });
});

describe("ContinueWatchingRail", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
  });

  it("renders resume items as direct-play landscape cards with progress and remaining time", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter>
          <ContinueWatchingRail
            items={[
              {
                id: "episode-1",
                title: "第一集",
                itemType: "EPISODE",
                rating: 8.4,
                ratingSource: "TMDb",
                runtimeTicks: 3_600_000_000,
                imageTags: { thumb: "episode-thumb-tag" },
                userData: { positionTicks: 1_200_000_000 },
              },
            ]}
          />
        </MemoryRouter>,
      );
    });

    expect(container.querySelector("h2")?.textContent).toBe("继续观看");
    expect(container.querySelector(".lux-horizontal-scroll-viewport")).not.toBeNull();
    expect(container.querySelector<HTMLAnchorElement>(".lux-continue-card")?.getAttribute("href")).toBe(
      "/watch/episode-1",
    );
    expect(container.querySelector(".lux-continue-card .lux-rating")).toBeNull();
    expect(container.querySelector(".lux-continue-card img")?.getAttribute("src"))
      .toBe("/api/v1/items/episode-1/images/thumb?tag=episode-thumb-tag");
    expect(container.querySelector(".lux-progress span")?.getAttribute("style")).toContain("width: 33%");
    expect(container.querySelector(".lux-continue-remaining")?.textContent).toBe("还剩 4m");
    expect(container.querySelector(".lux-continue-recency")).toBeNull();
  });

  it("shows the series title, formatted episode, and recent viewing for an episode", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const episode = {
      id: "episode-2",
      title: "用魔法打败魔法",
      itemType: "EPISODE" as const,
      seriesName: "我不是大师",
      parentIndexNumber: 1,
      indexNumber: 5,
      runtimeTicks: 36_000_000_000,
      userData: {
        positionTicks: 1_000_000_000,
        lastPlayedAt: Math.floor((Date.now() - 30 * 60 * 1000) / 1000),
      },
    };

    act(() => {
      root.render(
        <MemoryRouter>
          <ContinueWatchingRail items={[episode]} />
        </MemoryRouter>,
      );
    });

    expect(container.querySelector(".lux-continue-primary strong")?.textContent).toBe("我不是大师");
    expect(container.querySelector(".lux-continue-context")?.textContent).toBe("S01E05·用魔法打败魔法");
    expect(container.querySelector(".lux-continue-recency")?.textContent).toBe("半小时前看到这里");
  });

  it("shows a movie title, remaining time, year, and recent viewing on separate lines", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter>
          <ContinueWatchingRail items={[{
            id: "movie-continue",
            title: "蜘蛛侠：崭新之日",
            itemType: "MOVIE",
            productionYear: 2026,
            runtimeTicks: 72_000_000_000,
            userData: {
              positionTicks: 18_000_000_000,
              lastPlayedAt: Math.floor((Date.now() - 25 * 60 * 1000) / 1000),
            },
          }]} />
        </MemoryRouter>,
      );
    });

    expect(container.querySelector(".lux-continue-primary strong")?.textContent).toBe("蜘蛛侠：崭新之日");
    expect(container.querySelector(".lux-continue-remaining")?.textContent).toBe("还剩 1h 30m");
    expect(container.querySelector(".lux-continue-context")?.textContent).toBe("2026");
    expect(container.querySelector(".lux-continue-recency")?.textContent).toBe("25 分钟前看到这里");
  });

  it("uses warm and timezone-local relative playback copy", () => {
    const now = new Date(2026, 9, 17, 15, 0, 0);
    const at = (day: number, hour: number, minute = 0) =>
      Math.floor(new Date(2026, 9, day, hour, minute).getTime() / 1000);

    expect(recentPlaybackLabel(at(17, 14, 58), now.getTime())).toBe("刚才看到这里");
    expect(recentPlaybackLabel(at(17, 14, 30), now.getTime())).toBe("半小时前看到这里");
    expect(recentPlaybackLabel(at(17, 12, 30), now.getTime())).toBe("2 小时前看到这里");
    expect(recentPlaybackLabel(at(17, 9), now.getTime())).toBe("今天上午看到这里");
    expect(recentPlaybackLabel(at(16, 20), now.getTime())).toBe("昨天看到这里，接着来");
    expect(recentPlaybackLabel(at(13, 10), now.getTime())).toBe("4 天前看到这里");
    expect(recentPlaybackLabel(at(7, 10), now.getTime())).toBe("上次看到这里，回来接着看");
    expect(recentPlaybackLabel(null, now.getTime())).toBeUndefined();
    expect(recentPlaybackLabel(at(17, 16), now.getTime())).toBeUndefined();
  });

  it("shows HomeVideos progress and opens the video player from continue watching", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter>
          <ContinueWatchingRail items={[{
            id: "video-continue",
            title: "家庭录像",
            itemType: "VIDEO",
            runtimeTicks: 6_000_000_000,
            userData: { positionTicks: 2_400_000_000 },
          }]} />
        </MemoryRouter>,
      );
    });

    const card = container.querySelector<HTMLAnchorElement>(".lux-continue-card");
    expect(card?.getAttribute("href")).toBe("/watch/video-continue");
    expect(card?.querySelector(".lux-continue-copy small")?.textContent).toBe("其他视频");
    expect(card?.querySelector(".lux-progress span")?.getAttribute("style")).toContain("width: 40%");
  });

  it("shows latest media ratings as numeric TMDb-blue pills", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter>
          <MediaRail
            title="最新电影"
            items={[{ id: "movie-1", title: "示例电影", itemType: "MOVIE", rating: 7.6, ratingSource: "TMDb" }]}
          />
        </MemoryRouter>,
      );
    });

    const badge = container.querySelector(".lux-media-card .lux-rating");
    expect(container.querySelector(".lux-horizontal-scroll-viewport")).not.toBeNull();
    expect(badge?.textContent).toBe("7.6");
    expect(badge?.classList.contains("lux-rating")).toBe(true);
    expect(badge?.querySelector("svg")).toBeNull();
  });

  it("shows a series episode count on the opposite side of the poster rating", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter>
          <MediaCard
            item={{ id: "series-1", title: "示例剧集", itemType: "SERIES", rating: 8.1, episodeCount: 12 }}
          />
        </MemoryRouter>,
      );
    });

    expect(container.querySelector(".lux-card-rating")?.classList.contains("lux-rating")).toBe(true);
    expect(container.querySelector(".lux-media-episode-count")?.textContent).toBe("12 集");
    expect(container.querySelector(".lux-media-art")?.textContent).toContain("12 集");
  });

  it("shows a season episode count on the poster", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter>
          <MediaCard
            item={{ id: "season-1", title: "第一季", itemType: "SEASON", episodeCount: 8 }}
          />
        </MemoryRouter>,
      );
    });

    expect(container.querySelector(".lux-media-episode-count")?.textContent).toBe("8 集");
  });

  it("shows a waiting placeholder while local sidecars are being indexed", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter>
          <MediaCard
            item={{
              id: "pending-local-metadata",
              title: "正在读取本地资源",
              itemType: "MOVIE",
              localMetadataPending: true,
            }}
          />
        </MemoryRouter>,
      );
    });

    expect(container.querySelector(".lux-media-placeholder")?.textContent)
      .toContain("正在读取本地资源");
    expect(container.querySelector(".lux-media-placeholder-spinner")).not.toBeNull();
    expect(container.querySelector('[role="status"]')?.getAttribute("aria-label"))
      .toBe("正在读取本地海报和元数据");
  });

  it("hides the section when there are no resume items", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter>
          <ContinueWatchingRail items={[]} />
        </MemoryRouter>,
      );
    });

    expect(container.querySelector("section")).toBeNull();
    expect(container.textContent).toBe("");
  });
});
