// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it } from "vitest";
import { ContinueWatchingRail, MediaCard, MediaRail } from "../src/features/home/media";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

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
    expect(container.querySelector(".lux-continue-card .lux-rating")?.textContent).toBe("8.4");
    expect(container.querySelector(".lux-continue-card .lux-rating svg")).toBeNull();
    expect(container.querySelector(".lux-continue-card img")?.getAttribute("src"))
      .toBe("/api/v1/items/episode-1/images/thumb?tag=episode-thumb-tag");
    expect(container.querySelector(".lux-progress span")?.getAttribute("style")).toContain("width: 33%");
    expect(container.querySelector(".lux-continue-remaining")?.textContent).toBe("还剩 4m");
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
