import { describe, expect, it } from "vitest";
import type { MediaSource, WebPlaybackBitrate } from "../src/lib/api/types";
import {
  bitrateParamFromQueryKey,
  parseMaxBitrateParam,
  qualityOptions,
  qualityRateLabel,
  qualityStatusText,
  qualitySwitchParams,
  shouldShowQualitySelector,
  supportsQualitySelection,
} from "../src/features/player/playback-quality";

function source(bitrate: number | null | undefined, sourceKind = "LOCAL_FILE"): MediaSource {
  return { id: "source-1", sourceKind, bitrate };
}

function bitrate(overrides: Partial<WebPlaybackBitrate> = {}): WebPlaybackBitrate {
  return {
    sourceBitrate: 12_000_000,
    maxBitrate: null,
    targetBitrate: null,
    limited: false,
    ...overrides,
  };
}

describe("player quality tiers", () => {
  it("offers every tier when the source bitrate is unknown", () => {
    const options = qualityOptions(source(null), 4_000_000);

    expect(options.map((option) => option.value)).toEqual([
      "",
      "20000000",
      "8000000",
      "4000000",
      "2000000",
      "1000000",
    ]);
    expect(options[0]).toMatchObject({ label: "原画（自动）", bitrate: null, isCurrent: false });
    expect(options[3]).toMatchObject({ label: "4 Mbps", bitrate: 4_000_000, isCurrent: true });
  });

  it("only offers tiers below a known source bitrate", () => {
    expect(qualityOptions(source(12_000_000), null).map((option) => option.label))
      .toEqual(["原画（自动）", "8 Mbps", "4 Mbps", "2 Mbps", "1 Mbps"]);
    expect(qualityOptions(source(25_000_000), null).map((option) => option.label))
      .toEqual(["原画（自动）", "20 Mbps", "8 Mbps", "4 Mbps", "2 Mbps", "1 Mbps"]);
    expect(qualityOptions(source(4_000_000), null).map((option) => option.label))
      .toEqual(["原画（自动）", "2 Mbps", "1 Mbps"]);
  });

  it("marks the automatic choice and keeps a stale selection visible", () => {
    expect(qualityOptions(source(12_000_000), null)[0]?.isCurrent).toBe(true);
    // A saved 8 Mbps choice stays selectable even if the current source is 4 Mbps:
    // silently dropping it would rewrite the URL out from under the user.
    const options = qualityOptions(source(4_000_000), 8_000_000);
    expect(options.map((option) => option.value)).toContain("8000000");
    expect(options.find((option) => option.value === "8000000")?.isCurrent).toBe(true);
  });

  it("never offers a higher tier than the source already delivers", () => {
    const options = qualityOptions(source(5_000_000), null);
    expect(options.some((option) => (option.bitrate ?? 0) >= 5_000_000)).toBe(false);
  });

  it("hides the selector when there is nothing to cap", () => {
    // 800 kbps source: every tier is above it, so only the automatic choice remains.
    expect(qualityOptions(source(800_000), null).map((option) => option.label)).toEqual(["原画（自动）"]);
    expect(shouldShowQualitySelector(qualityOptions(source(800_000), null))).toBe(false);
    expect(shouldShowQualitySelector(qualityOptions(source(12_000_000), null))).toBe(true);
  });

  it("formats rates without trailing zeros", () => {
    expect(qualityRateLabel(20_000_000)).toBe("20 Mbps");
    expect(qualityRateLabel(4_000_000)).toBe("4 Mbps");
    expect(qualityRateLabel(3_500_000)).toBe("3.5 Mbps");
    expect(qualityRateLabel(800_000)).toBe("800 kbps");
    expect(qualityRateLabel(null)).toBeNull();
    expect(qualityRateLabel(0)).toBeNull();
    expect(qualityRateLabel(Number.NaN)).toBeNull();
  });
});

describe("player maxBitrate parameter", () => {
  it("accepts only the range the API validates", () => {
    expect(parseMaxBitrateParam("4000000")).toBe(4_000_000);
    expect(parseMaxBitrateParam(" 100000 ")).toBe(100_000);
    expect(parseMaxBitrateParam("200000000")).toBe(200_000_000);
    expect(parseMaxBitrateParam("")).toBeNull();
    expect(parseMaxBitrateParam(null)).toBeNull();
    expect(parseMaxBitrateParam("99999")).toBeNull();
    expect(parseMaxBitrateParam("200000001")).toBeNull();
    expect(parseMaxBitrateParam("4.5")).toBeNull();
    expect(parseMaxBitrateParam("abc")).toBeNull();
  });

  it("writes the cap to the URL and removes it for the automatic choice", () => {
    const current = new URLSearchParams({ sourceId: "source-1", audioStreamIndex: "2" });

    const capped = qualitySwitchParams(current, 4_000_000);
    expect(capped.get("maxBitrate")).toBe("4000000");
    expect(capped.get("sourceId")).toBe("source-1");
    expect(capped.get("audioStreamIndex")).toBe("2");
    // The input parameters are not mutated.
    expect(current.get("maxBitrate")).toBeNull();

    const automatic = qualitySwitchParams(capped, null);
    expect(automatic.get("maxBitrate")).toBeNull();
    expect(automatic.get("audioStreamIndex")).toBe("2");
  });

  it("reads the cap from both playback query key layouts", () => {
    expect(bitrateParamFromQueryKey(
      ["playback-bootstrap", "item", "source", "2", "7", "4000000", 0],
    )).toBe("4000000");
    expect(bitrateParamFromQueryKey(
      ["web-playback-session", "item", "source", "2", "7", "4000000", 1],
    )).toBe("4000000");
    expect(bitrateParamFromQueryKey(
      ["web-playback-session", "item", "source", "2", "7", "default", 1],
    )).toBe("default");
  });
});

describe("player quality status", () => {
  it("reports the effective limit from the session response", () => {
    expect(qualityStatusText(bitrate({ maxBitrate: 4_000_000, targetBitrate: 4_000_000, limited: true }), 4_000_000))
      .toBe("已限制到 4 Mbps");
    expect(qualityStatusText(bitrate({ targetBitrate: 3_500_000, limited: true }), 4_000_000))
      .toBe("已限制到 3.5 Mbps");
    // The administrator cap limits the output even without a user choice.
    expect(qualityStatusText(bitrate({ maxBitrate: null, targetBitrate: 8_000_000, limited: true }), null))
      .toBe("已限制到 8 Mbps");
  });

  it("reports the original stream when nothing is capped", () => {
    expect(qualityStatusText(bitrate(), null)).toBe("原画（自动）");
    expect(qualityStatusText(undefined, null)).toBe("原画（自动）");
    expect(qualityStatusText(bitrate({ maxBitrate: 4_000_000 }), 4_000_000))
      .toBe("原画（源码率未超过所选上限）");
    // An unprobed source never triggers an upgrade, so the ceiling is not
    // exceeded — and the wording must not claim the source rate is known.
    expect(qualityStatusText(bitrate({ sourceBitrate: null, maxBitrate: 4_000_000 }), 4_000_000))
      .toBe("原画（源码率未知，未触发限制）");
  });
});

describe("player quality availability", () => {
  it("does not offer a cap for STRM sources", () => {
    expect(supportsQualitySelection(source(12_000_000))).toBe(true);
    expect(supportsQualitySelection(source(12_000_000, "STRM_URL"))).toBe(false);
    expect(supportsQualitySelection(undefined)).toBe(true);
    expect(shouldShowQualitySelector(qualityOptions(source(12_000_000, "STRM_URL"), null))).toBe(false);
  });
});
