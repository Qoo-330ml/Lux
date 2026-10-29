// @vitest-environment jsdom

import { act } from "react";
import { chooseLuxOptionByValue, luxCombobox, luxOptions } from "./lux-select-helpers";
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

    const seasonSwitch = container!.querySelector(
      'input[role="switch"][aria-label="季度海报开关"]',
    ) as HTMLInputElement;
    expect(seasonSwitch).toBeTruthy();
    expect(seasonSwitch.checked).toBe(true);
    act(() => {
      seasonSwitch.click();
    });

    expect(latest.levels?.season?.artworks?.poster?.enabled).toBe(false);
    expect(latest.levels?.series).toBeUndefined();
  });

  it("shows the episode image choice only on the episode row", () => {
    render(<ImageLevelPolicyEditor images={base} onChange={() => {}} />);

    const groups = Array.from(container!.querySelectorAll("details"));
    const labels = groups.map((group) => group.querySelector("summary")?.textContent ?? "");
    expect(labels.some((label) => label.includes("单集"))).toBe(true);
    expect(luxCombobox(container!, "单集图片来源")).toBeTruthy();
    expect(luxCombobox(container!, "电影图片来源")).toBeNull();
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

  it("writes the selected count tier", async () => {
    let latest: MediaImageStrategySettings = base;
    render(
      <ImageLevelPolicyEditor
        images={base}
        onChange={(next) => {
          latest = next;
        }}
      />,
    );

    expect(luxCombobox(container!, "季度海报张数")?.tagName).toBe("BUTTON");
    await chooseLuxOptionByValue(container!, "季度海报张数", "2");

    expect(latest.levels?.season?.artworks?.poster?.maxCount).toBe(2);
  });

  it("writes the selected width tier", async () => {
    let latest: MediaImageStrategySettings = base;
    render(
      <ImageLevelPolicyEditor
        images={base}
        onChange={(next) => {
          latest = next;
        }}
      />,
    );

    await chooseLuxOptionByValue(container!, "单集背景图最小宽度", "1920");

    expect(latest.levels?.episode?.artworks?.backdrop?.minWidth).toBe(1920);
  });
});
describe("inherit label follows the settings level", () => {
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

  it("uses 默认 when the global strategy has no parent layer", async () => {
    render(
      <ImageLevelPolicyEditor
        images={base}
        inheritLabel="默认"
        emptySummary="全部默认"
        onChange={() => {}}
      />,
    );

    const enabled = container!.querySelector(
      'input[role="switch"][aria-label="电影海报开关"]',
    ) as HTMLInputElement;
    expect(enabled.checked).toBe(true);
    expect(
      enabled.parentElement?.querySelector(".lux-level-policy-inherit")?.textContent,
    ).toBe("默认");

    const count = await luxOptions(container!, "电影海报张数");
    expect(count[0]?.label).toBe("默认");

    const width = await luxOptions(container!, "电影海报最小宽度");
    expect(width[0]?.label).toBe("默认");

    expect(container!.textContent).toContain("全部默认");
    expect(container!.textContent).not.toContain("全部继承");
  });

  it("keeps 继承 for a library override panel", () => {
    render(<ImageLevelPolicyEditor images={base} onChange={() => {}} />);

    const enabled = container!.querySelector(
      'input[role="switch"][aria-label="电影海报开关"]',
    ) as HTMLInputElement;
    expect(
      enabled.parentElement?.querySelector(".lux-level-policy-inherit")?.textContent,
    ).toBe("继承");
    expect(container!.textContent).toContain("全部继承");
  });
});
describe("level switch is a plain on/off toggle", () => {
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

  it("follows the flat switch until it is toggled", () => {
    render(<ImageLevelPolicyEditor images={{ ...base, poster: false }} onChange={() => {}} />);
    const poster = container!.querySelector(
      'input[role="switch"][aria-label="电影海报开关"]',
    ) as HTMLInputElement;
    expect(poster.checked).toBe(false);
  });

  it("clears the explicit value back to the inherited state", () => {
    let latest: MediaImageStrategySettings = base;
    const withOverride = updateArtworkPolicy(base, "series", "poster", { enabled: false });
    render(
      <ImageLevelPolicyEditor
        images={withOverride}
        onChange={(next) => {
          latest = next;
        }}
      />,
    );

    const reset = container!.querySelector(
      'button[aria-label="剧集海报恢复继承"]',
    ) as HTMLButtonElement;
    expect(reset).toBeTruthy();
    act(() => {
      reset.click();
    });

    expect(latest.levels).toBeUndefined();
  });

  it("marks explicitly set cells without a hint", () => {
    const withOverride = updateArtworkPolicy(base, "movie", "logo", { enabled: true });
    render(<ImageLevelPolicyEditor images={withOverride} onChange={() => {}} />);
    const logo = container!.querySelector(
      'input[role="switch"][aria-label="电影徽标开关"]',
    ) as HTMLInputElement;
    expect(logo.checked).toBe(true);
    expect(logo.parentElement?.querySelector(".lux-level-policy-inherit")).toBeNull();
  });
});
