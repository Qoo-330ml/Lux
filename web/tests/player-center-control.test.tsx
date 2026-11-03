// @vitest-environment jsdom

import { act } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  LUX_MARK_SHAPE,
  PAUSE_BARS_SHAPE,
  PlayerCenterControl,
  interpolateShape,
  shapeToPath,
} from "../src/features/player/components/player-center-control";
import { PlayerTopBar } from "../src/features/player/components/player-top-bar";
import { selectLogoUrl, titleLogoOwnerId } from "../src/features/player/usePlayerTitleLogo";
import type { MediaItem } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const facetPaths = (host: Element) => Array.from(host.querySelectorAll("svg path")).map((path) => path.getAttribute("d") ?? "");

describe("shape helpers", () => {
  it("interpolates point by point between the Lux mark and the pause bars", () => {
    expect(interpolateShape(LUX_MARK_SHAPE, PAUSE_BARS_SHAPE, 0)).toEqual(LUX_MARK_SHAPE);
    expect(interpolateShape(LUX_MARK_SHAPE, PAUSE_BARS_SHAPE, 1)).toEqual(PAUSE_BARS_SHAPE);
    const middle = interpolateShape(LUX_MARK_SHAPE, PAUSE_BARS_SHAPE, 0.5);
    expect(middle[2]?.[0]).toEqual([(445 + 310) / 2, (185 + 40) / 2]);
  });

  it("keeps the same point count in every facet so shapes can always morph", () => {
    for (const shape of [LUX_MARK_SHAPE, PAUSE_BARS_SHAPE]) {
      expect(shape).toHaveLength(3);
      for (const facet of shape) expect(facet).toHaveLength(5);
    }
  });

  it("serialises a facet to a closed SVG path", () => {
    expect(shapeToPath([[1, 2], [3, 4]])).toBe("M1.0 2.0 L3.0 4.0 Z");
  });
});

describe("PlayerCenterControl", () => {
  let container: HTMLDivElement | null = null;
  let root: Root | null = null;

  afterEach(async () => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
    if (root) await act(async () => root?.unmount());
    container?.remove();
    container = null;
    root = null;
  });

  async function mount(props: { playing: boolean; loading?: boolean; onClick?: () => void }) {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const render = async (next: { playing: boolean; loading?: boolean; onClick?: () => void }) => {
      await act(async () => {
        root?.render(<PlayerCenterControl playing={next.playing} loading={next.loading} onClick={next.onClick ?? (() => undefined)} />);
      });
    };
    await render(props);
    return { host: container, render };
  }

  it("shows the Lux mark and a play label while paused", () => {
    const markup = renderToStaticMarkup(<PlayerCenterControl playing={false} onClick={() => undefined} />);
    expect(markup).toContain('aria-label="播放"');
    expect(markup).toContain("M445.0 185.0");
    expect(markup).not.toContain("is-playing");
  });

  it("starts as two bars with a pause label while playing", () => {
    const markup = renderToStaticMarkup(<PlayerCenterControl playing onClick={() => undefined} />);
    expect(markup).toContain('aria-label="暂停"');
    expect(markup).toContain("M310.0 40.0");
    expect(markup).toContain("is-playing");
  });

  it("keeps the mark and announces loading while a stream is being prepared", () => {
    const markup = renderToStaticMarkup(<PlayerCenterControl playing loading onClick={() => undefined} />);
    expect(markup).toContain('aria-label="加载中"');
    expect(markup).toContain("is-loading");
    expect(markup).toContain("M445.0 185.0");
  });

  it("calls onClick", async () => {
    const onClick = vi.fn();
    const { host } = await mount({ playing: false, onClick });
    await act(async () => host.querySelector("button")?.click());
    expect(onClick).toHaveBeenCalledTimes(1);
  });

  it("animates from the mark to the bars when playback starts, and back on pause", async () => {
    vi.useFakeTimers({ toFake: ["requestAnimationFrame", "cancelAnimationFrame", "performance"] });
    const { host, render } = await mount({ playing: false });
    const mark = facetPaths(host);

    await render({ playing: true });
    await act(async () => vi.advanceTimersByTime(240));
    const middle = facetPaths(host);
    expect(middle).not.toEqual(mark);
    expect(middle[2]).not.toContain("M310.0 40.0");

    await act(async () => vi.advanceTimersByTime(400));
    expect(facetPaths(host)[2]).toContain("M310.0 40.0");

    await render({ playing: false });
    await act(async () => vi.advanceTimersByTime(600));
    expect(facetPaths(host)).toEqual(mark);
  });

  it("switches instantly when the user prefers reduced motion", async () => {
    vi.stubGlobal("matchMedia", (query: string) => ({ matches: query.includes("reduce"), media: query, addEventListener: () => undefined, removeEventListener: () => undefined }));
    const { host, render } = await mount({ playing: false });
    await render({ playing: true });
    expect(facetPaths(host)[2]).toContain("M310.0 40.0");
  });
});

describe("PlayerTopBar title", () => {
  const noop = () => undefined;

  it("shows the logo instead of the text title, with the year on its right", () => {
    const markup = renderToStaticMarkup(<PlayerTopBar title="飞驰人生" logoUrl="/api/v1/items/1/images/logo" subtitle="2019" onBack={noop} />);
    expect(markup).toContain('class="lux-player-title-logo"');
    expect(markup).toContain('alt="飞驰人生"');
    expect(markup).not.toContain('class="lux-player-title"');
    expect(markup.indexOf("lux-player-title-logo")).toBeLessThan(markup.indexOf("lux-player-year"));
    expect(markup).toContain("2019");
  });

  it("shows only the text title and the year when there is no logo", () => {
    const markup = renderToStaticMarkup(<PlayerTopBar title="飞驰人生" subtitle="2019" onBack={noop} />);
    expect(markup).not.toContain("lux-player-title-logo");
    expect(markup).toContain('<span class="lux-player-title">飞驰人生</span><span class="lux-player-year">2019</span>');
    expect(markup).not.toContain("1080");
  });

  it("falls back to the text title when the logo fails to load", async () => {
    const host = document.createElement("div");
    document.body.append(host);
    const root = createRoot(host);
    await act(async () => root.render(<PlayerTopBar title="示例剧集" logoUrl="/broken.png" subtitle="第 1 季 第 2 集" onBack={noop} />));
    expect(host.querySelector(".lux-player-title-logo")).not.toBeNull();
    await act(async () => host.querySelector(".lux-player-title-logo")?.dispatchEvent(new Event("error")));
    expect(host.querySelector(".lux-player-title-logo")).toBeNull();
    expect(host.querySelector(".lux-player-title")?.textContent).toBe("示例剧集");
    expect(host.querySelector(".lux-player-year")?.textContent).toBe("第 1 季 第 2 集");
    await act(async () => root.unmount());
    host.remove();
  });
});

describe("title logo lookup", () => {
  const item = (fields: Partial<MediaItem>) => ({ id: "item-1", ...fields }) as MediaItem;

  it("uses the series logo for episodes and the item's own logo otherwise", () => {
    expect(titleLogoOwnerId(item({ itemType: "EPISODE", seriesId: "series-9" }))).toBe("series-9");
    expect(titleLogoOwnerId(item({ itemType: "EPISODE", seriesId: null }))).toBe("item-1");
    expect(titleLogoOwnerId(item({ itemType: "MOVIE" }))).toBe("item-1");
    expect(titleLogoOwnerId(null)).toBeNull();
  });

  it("picks the LOGO image whatever its casing", () => {
    expect(selectLogoUrl({ images: [{ imageType: "poster", url: "/p" }, { imageType: "Logo", url: "/l" }] } as never)).toBe("/l");
    expect(selectLogoUrl({ images: [{ imageType: "poster", url: "/p" }] } as never)).toBeNull();
    expect(selectLogoUrl({} as never)).toBeNull();
  });
});
