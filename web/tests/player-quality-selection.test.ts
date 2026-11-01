import { describe, expect, it } from "vitest";
import type { MediaSource, WebPlaybackResolution } from "../src/lib/api/types";
import {
  canonicalMaxHeightParam,
  maxHeightParamFromQueryKey,
  parseMaxHeightParam,
  qualityOptions,
  qualityStatusText,
  qualitySwitchParams,
  shouldShowQualitySelector,
  sourceVideoHeight,
  supportsQualitySelection,
} from "../src/features/player/playback-quality";

function source(
  height: number | string | null | undefined,
  sourceKind = "LOCAL_FILE",
): MediaSource {
  return {
    id: "source-1",
    sourceKind,
    streams: [
      {
        index: 0,
        type: "VIDEO",
        details: height === null || height === undefined ? {} : { Height: height },
      },
      { index: 1, type: "AUDIO", codec: "aac" },
    ],
  };
}

function resolution(overrides: Partial<WebPlaybackResolution> = {}): WebPlaybackResolution {
  return {
    sourceHeight: 2160,
    maxHeight: null,
    targetHeight: null,
    limited: false,
    ...overrides,
  };
}

describe("player resolution tiers", () => {
  it("offers every tier when the source height is unknown", () => {
    const options = qualityOptions(source(null), 720);

    expect(options.map((option) => option.value)).toEqual([
      "",
      "2160",
      "1440",
      "1080",
      "720",
      "480",
      "360",
    ]);
    expect(options[0]).toMatchObject({ label: "原画（自动）", height: null, isCurrent: false });
    expect(options[4]).toMatchObject({ label: "720p", height: 720, isCurrent: true });
    expect(options[1]?.label).toBe("2160p（4K）");
  });

  it("only offers tiers strictly below a known source height", () => {
    expect(qualityOptions(source(2160), null).map((option) => option.label))
      .toEqual(["原画（自动）", "1440p", "1080p", "720p", "480p", "360p"]);
    expect(qualityOptions(source(1080), null).map((option) => option.label))
      .toEqual(["原画（自动）", "720p", "480p", "360p"]);
    expect(qualityOptions(source("720"), null).map((option) => option.label))
      .toEqual(["原画（自动）", "480p", "360p"]);
  });

  it("marks the automatic choice and the selected tier", () => {
    expect(qualityOptions(source(2160), null)[0]?.isCurrent).toBe(true);
    const options = qualityOptions(source(2160), 720);
    expect(options[0]?.isCurrent).toBe(false);
    expect(options.find((option) => option.value === "720")?.isCurrent).toBe(true);
  });

  it("never offers the source height itself or anything above it", () => {
    const options = qualityOptions(source(1080), null);
    expect(options.some((option) => (option.height ?? 0) >= 1080)).toBe(false);
  });

  it("keeps a saved ceiling the current source cannot offer", () => {
    // 4K 源选了 1440p 后切到 1080p 源：1440p 不再低于源高度，但仍是 URL 里的
    // 真实选择，必须继续显示并标为当前，用户才能看到并切回「原画（自动）」。
    const options = qualityOptions(source(1080), 1440);
    expect(options.map((option) => option.value)).toEqual(["", "1440", "720", "480", "360"]);
    expect(options[0]?.isCurrent).toBe(false);
    expect(options.find((option) => option.value === "1440"))
      .toMatchObject({ label: "1440p", height: 1440, isCurrent: true });
  });

  it("keeps a valid hand-written ceiling outside the named tiers", () => {
    const options = qualityOptions(source(2160), 900);
    expect(options.map((option) => option.value)).toContain("900");
    expect(options.find((option) => option.value === "900"))
      .toMatchObject({ label: "900p", height: 900, isCurrent: true });
    expect(options[0]?.isCurrent).toBe(false);
  });

  it("hides the selector when the source has no lower tier", () => {
    expect(qualityOptions(source(360), null).map((option) => option.label)).toEqual(["原画（自动）"]);
    expect(shouldShowQualitySelector(qualityOptions(source(360), null))).toBe(false);
    expect(shouldShowQualitySelector(qualityOptions(source(1080), null))).toBe(true);
    expect(shouldShowQualitySelector(qualityOptions(source(null), null))).toBe(true);
  });

  it("reads the video stream height from the probe details", () => {
    expect(sourceVideoHeight(source(2160))).toBe(2160);
    expect(sourceVideoHeight(source("1080"))).toBe(1080);
    expect(sourceVideoHeight({ ...source(720), streams: [{ index: 0, type: "VIDEO" }] })).toBeNull();
    expect(sourceVideoHeight(source(0))).toBeNull();
    expect(sourceVideoHeight(source("abc"))).toBeNull();
    expect(sourceVideoHeight(undefined)).toBeNull();
  });
});

describe("player maxHeight parameter", () => {
  it("accepts only the range the API validates", () => {
    expect(parseMaxHeightParam("720")).toBe(720);
    expect(parseMaxHeightParam(" 144 ")).toBe(144);
    expect(parseMaxHeightParam("4320")).toBe(4320);
    expect(parseMaxHeightParam("")).toBeNull();
    expect(parseMaxHeightParam(null)).toBeNull();
    expect(parseMaxHeightParam("143")).toBeNull();
    expect(parseMaxHeightParam("4321")).toBeNull();
    expect(parseMaxHeightParam("1080.5")).toBeNull();
    expect(parseMaxHeightParam("abc")).toBeNull();
  });

  it("writes the ceiling to the URL and removes it for the automatic choice", () => {
    const current = new URLSearchParams({ sourceId: "source-1", audioStreamIndex: "2" });

    const capped = qualitySwitchParams(current, 720);
    expect(capped.get("maxHeight")).toBe("720");
    expect(capped.get("sourceId")).toBe("source-1");
    expect(capped.get("audioStreamIndex")).toBe("2");
    // The input parameters are not mutated.
    expect(current.get("maxHeight")).toBeNull();

    const automatic = qualitySwitchParams(capped, null);
    expect(automatic.get("maxHeight")).toBeNull();
    expect(automatic.get("audioStreamIndex")).toBe("2");
  });

  it("does not read or write the legacy maxBitrate parameter", () => {
    const current = new URLSearchParams({ sourceId: "source-1", maxBitrate: "4000000" });

    // The legacy parameter is ignored: it is neither rewritten nor removed here.
    expect(qualitySwitchParams(current, 720).get("maxBitrate")).toBe("4000000");
    expect(qualitySwitchParams(current, 720).get("maxHeight")).toBe("720");
  });

  it("canonicalizes hand-written URL forms for cache variant matching", () => {
    // Query keys store the parsed number, so padded or exponent notation must be
    // reduced to the same string or the current variant looks like "another" one.
    expect(canonicalMaxHeightParam(" 720 ")).toBe("720");
    expect(canonicalMaxHeightParam("1.08e3")).toBe("1080");
    expect(canonicalMaxHeightParam("+480")).toBe("480");
    expect(canonicalMaxHeightParam("720.0")).toBe("720");
    expect(canonicalMaxHeightParam("")).toBeNull();
    expect(canonicalMaxHeightParam(null)).toBeNull();
    expect(canonicalMaxHeightParam("1e9")).toBeNull();
    expect(canonicalMaxHeightParam("abc")).toBeNull();
  });

  it("reads the ceiling from both playback query key layouts", () => {
    expect(maxHeightParamFromQueryKey(
      ["playback-bootstrap", "item", "source", "2", "7", "720", 0],
    )).toBe("720");
    expect(maxHeightParamFromQueryKey(
      ["web-playback-session", "item", "source", "2", "7", "720", 1],
    )).toBe("720");
    expect(maxHeightParamFromQueryKey(
      ["web-playback-session", "item", "source", "2", "7", "default", 1],
    )).toBe("default");
  });
});

describe("player quality status", () => {
  it("reports the effective downscale from the session response", () => {
    expect(qualityStatusText(resolution({ maxHeight: 720, targetHeight: 720, limited: true })))
      .toBe("已缩放到 720p");
    expect(qualityStatusText(resolution({ maxHeight: 1080, targetHeight: 1080, limited: true })))
      .toBe("已缩放到 1080p");
  });

  it("reports the original stream when nothing was scaled", () => {
    expect(qualityStatusText(resolution({ sourceHeight: 1080 }))).toBe("原画 1080p");
    expect(qualityStatusText(resolution({ sourceHeight: 2160, targetHeight: 2160 })))
      .toBe("原画 2160p");
    // `limited: false` reports what the server really emits: the target height
    // wins over the source height, which may differ (e.g. anamorphic sources).
    expect(qualityStatusText(resolution({ sourceHeight: 2160, targetHeight: 1920, limited: false })))
      .toBe("原画 1920p");
    expect(qualityStatusText(resolution({ sourceHeight: 2160, targetHeight: null, limited: false })))
      .toBe("原画 2160p");
  });

  it("stays neutral when the server does not report a resolution block", () => {
    // An older server sends no `resolution` at all: the player cannot claim the
    // ceiling was accepted or which height is playing.
    expect(qualityStatusText(undefined)).toBe("原画（自动）");
    expect(qualityStatusText(null)).toBe("原画（自动）");
    // Individual fields can be null too; without a height there is nothing to claim.
    expect(qualityStatusText(resolution({ sourceHeight: null, targetHeight: null })))
      .toBe("原画（自动）");
    expect(qualityStatusText(resolution({ sourceHeight: null, maxHeight: 720, limited: true })))
      .toBe("原画（自动）");
    // `limited` without a target height must not be reported as "原画 <source>".
    expect(qualityStatusText(resolution({ sourceHeight: 2160, maxHeight: 720, limited: true })))
      .toBe("原画（自动）");
    // A null `limited` is also "not reported": no conclusion about the output.
    expect(qualityStatusText(resolution({ sourceHeight: 2160, targetHeight: 2160, limited: null })))
      .toBe("原画（自动）");
  });
});

describe("player quality availability", () => {
  it("does not offer a resolution ceiling for STRM sources", () => {
    expect(supportsQualitySelection(source(2160))).toBe(true);
    expect(supportsQualitySelection(source(2160, "STRM_URL"))).toBe(false);
    expect(supportsQualitySelection(undefined)).toBe(true);
    expect(shouldShowQualitySelector(qualityOptions(source(2160, "STRM_URL"), null))).toBe(false);
  });
});
