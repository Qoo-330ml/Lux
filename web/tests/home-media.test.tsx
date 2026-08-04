// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it } from "vitest";
import { ContinueWatchingRail } from "../src/features/home/media";

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
                runtimeTicks: 3_600_000_000,
                imageTags: { fanart: "fanart-tag" },
                userData: { positionTicks: 1_200_000_000 },
              },
            ]}
          />
        </MemoryRouter>,
      );
    });

    expect(container.querySelector("h2")?.textContent).toBe("继续观看");
    expect(container.querySelector<HTMLAnchorElement>(".lux-continue-card")?.getAttribute("href")).toBe(
      "/watch/episode-1",
    );
    expect(container.querySelector(".lux-progress span")?.getAttribute("style")).toContain("width: 33%");
    expect(container.querySelector(".lux-continue-remaining")?.textContent).toBe("还剩 4m");
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
