// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it } from "vitest";
import {
  ImageLevelPolicyEditor,
  updateArtworkPolicy,
  updateLevelEpisodePolicy,
} from "../src/features/admin/ImageLevelPolicyEditor";
import {
  countPresets,
  flatCountOptions,
  flatWidthOptions,
  presetsWithCurrent,
  widthPresets,
} from "../src/features/admin/image-policy-presets";
import type { MediaImageStrategySettings } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

const base: MediaImageStrategySettings = {
  poster: true,
  backdrop: true,
  artwork: false,
  banner: false,
  logo: true,
  thumbnail: true,
  thumbnailScrapingMode: "SCRAPER_FIRST",
  disc: false,
  wallpaper: false,
  writeToMetadata: false,
  maxBackdropCount: 4,
  minDownloadWidth: 1280,
};

describe("image level policy helpers", () => {
  it("writes a season-only override without mutating the input", () => {
    const next = updateArtworkPolicy(base, "season", "poster", {
      enabled: false,
      overwrite: "KEEP",
      maxCount: 1,
      minWidth: 720,
      sources: ["SCRAPER"],
      scraperRoles: ["PRIMARY"],
    });

    expect(next.levels?.season?.artworks?.poster).toEqual({
      enabled: false,
      overwrite: "KEEP",
      maxCount: 1,
      minWidth: 720,
      sources: ["SCRAPER"],
      scraperRoles: ["PRIMARY"],
    });
    expect(next.levels?.movie).toBeUndefined();
    expect(base.levels).toBeUndefined();
  });

  it("drops the level again once every value is cleared", () => {
    const withOverride = updateArtworkPolicy(base, "movie", "backdrop", { enabled: false });
    expect(withOverride.levels?.movie?.artworks?.backdrop).toEqual({ enabled: false });

    const cleared = updateArtworkPolicy(withOverride, "movie", "backdrop", {
      enabled: undefined,
    });
    expect(cleared.levels).toBeUndefined();
  });

  it("keeps the three-way episode policy and prunes empty levels", () => {
    const withPolicy = updateLevelEpisodePolicy(base, "episode", "LOCAL_SCREENSHOT");
    expect(withPolicy.levels?.episode?.episodeImagePolicy).toBe("LOCAL_SCREENSHOT");

    const cleared = updateLevelEpisodePolicy(withPolicy, "episode", "");
    expect(cleared.levels).toBeUndefined();
  });
});

describe("image level policy editor", () => {
  let container: HTMLDivElement | null = null;
  let root: Root | null = null;

  afterEach(() => {
    if (root) act(() => root!.unmount());
    container?.remove();
    root = null;
    container = null;
  });

  function render(node: React.ReactNode) {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    act(() => root!.render(node));
  }

  it("emits a season poster override when the matrix control changes", () => {
    let latest: MediaImageStrategySettings = base;
    render(
      <ImageLevelPolicyEditor
        images={base}
        onChange={(next) => {
          latest = next;
        }}
      />,
    );

    const seasonSelect = container!.querySelector(
      'select[aria-label="季度海报开关"]',
    ) as HTMLSelectElement;
    expect(seasonSelect).toBeTruthy();
    seasonSelect.value = "false";
    act(() => {
      seasonSelect.dispatchEvent(new Event("change", { bubbles: true }));
    });

    expect(latest.levels?.season?.artworks?.poster?.enabled).toBe(false);
    expect(latest.levels?.series).toBeUndefined();
  });

  it("shows the episode image choice only on the episode row", () => {
    render(<ImageLevelPolicyEditor images={base} onChange={() => {}} />);

    const groups = Array.from(container!.querySelectorAll("details"));
    const labels = groups.map((group) => group.querySelector("summary")?.textContent ?? "");
    expect(labels.some((label) => label.includes("单集"))).toBe(true);
    expect(container!.querySelectorAll("select[aria-label='单集图片来源']")).toHaveLength(1);
    expect(container!.querySelectorAll("select[aria-label='电影图片来源']")).toHaveLength(0);
  });
});
describe("image policy preset tiers", () => {
  it("offers wheel-friendly tiers instead of free typing", () => {
    expect(presetsWithCurrent(countPresets, undefined).map((option) => option.value)).toEqual([
      "",
      "0",
      "1",
      "2",
      "3",
      "4",
    ]);
    expect(presetsWithCurrent(widthPresets, undefined).map((option) => option.value)).toEqual([
      "",
      "0",
      "360",
      "720",
      "1080",
      "1280",
      "1920",
      "2160",
      "3840",
    ]);
  });

  it("keeps an existing non-tier value selectable", () => {
    const options = presetsWithCurrent(widthPresets, 640);
    expect(options.map((option) => option.value)).toContain("640");
    expect(options.at(-1)).toEqual({ value: "640", label: "当前 640" });
  });

  it("drops the inherit option for flat settings", () => {
    expect(flatCountOptions(4).map((option) => option.value)).toEqual(["0", "1", "2", "3", "4"]);
    expect(flatWidthOptions(1280).map((option) => option.value)).not.toContain("");
  });
});

describe("image level policy editor preset controls", () => {
  let container: HTMLDivElement | null = null;
  let root: Root | null = null;

  afterEach(() => {
    if (root) act(() => root!.unmount());
    container?.remove();
    root = null;
    container = null;
  });

  function render(node: React.ReactNode) {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    act(() => root!.render(node));
  }

  it("writes the selected count tier", () => {
    let latest: MediaImageStrategySettings = base;
    render(
      <ImageLevelPolicyEditor
        images={base}
        onChange={(next) => {
          latest = next;
        }}
      />,
    );

    const select = container!.querySelector(
      'select[aria-label="季度海报张数"]',
    ) as HTMLSelectElement;
    expect(select.tagName).toBe("SELECT");
    select.value = "2";
    act(() => {
      select.dispatchEvent(new Event("change", { bubbles: true }));
    });

    expect(latest.levels?.season?.artworks?.poster?.maxCount).toBe(2);
  });

  it("writes the selected width tier", () => {
    let latest: MediaImageStrategySettings = base;
    render(
      <ImageLevelPolicyEditor
        images={base}
        onChange={(next) => {
          latest = next;
        }}
      />,
    );

    const select = container!.querySelector(
      'select[aria-label="单集背景图最小宽度"]',
    ) as HTMLSelectElement;
    select.value = "1920";
    act(() => {
      select.dispatchEvent(new Event("change", { bubbles: true }));
    });

    expect(latest.levels?.episode?.artworks?.backdrop?.minWidth).toBe(1920);
  });
});
