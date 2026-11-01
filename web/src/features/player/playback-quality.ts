import type { MediaSource, WebPlaybackBitrate } from "../../lib/api/types";

/**
 * Pure model for the player's manual quality (bitrate ceiling) selector.
 *
 * The backend accepts an optional `maxBitrate` (bps) on the playback session and
 * bootstrap requests and answers with a `bitrate` summary describing what it
 * actually decided. Everything here is display/URL derivation only: it never
 * decides whether a source must be transcoded, it just proposes ceilings the
 * server is allowed to reject.
 */

/** Same inclusive range the API validates (`400 INVALID_REQUEST` outside it). */
export const MAX_BITRATE_MIN = 100_000;
export const MAX_BITRATE_MAX = 200_000_000;

/** Ceilings offered in the panel, highest first. */
export const QUALITY_TIER_BPS = [
  20_000_000,
  8_000_000,
  4_000_000,
  2_000_000,
  1_000_000,
] as const;

export type QualityOption = {
  /**
   * Value written to the `maxBitrate` URL parameter. The empty string is the
   * automatic choice, which is expressed by an absent parameter.
   */
  value: string;
  label: string;
  /** `null` for the automatic choice. */
  bitrate: number | null;
  isCurrent: boolean;
};

/** Human readable rate; whole megabits drop the decimal so tiers read "4 Mbps". */
export function qualityRateLabel(bitsPerSecond: number | null | undefined): string | null {
  if (typeof bitsPerSecond !== "number" || !Number.isFinite(bitsPerSecond) || bitsPerSecond <= 0) {
    return null;
  }
  if (bitsPerSecond < 1_000_000) return `${Math.round(bitsPerSecond / 1_000)} kbps`;
  const megabits = bitsPerSecond / 1_000_000;
  return Number.isInteger(megabits) ? `${megabits} Mbps` : `${megabits.toFixed(1)} Mbps`;
}

export function isValidMaxBitrate(value: number): boolean {
  return Number.isInteger(value) && value >= MAX_BITRATE_MIN && value <= MAX_BITRATE_MAX;
}

/**
 * Parses a `maxBitrate` URL value. An absent, blank or out-of-contract value is
 * treated as "no ceiling" so a hand-edited URL can never make the player send a
 * request the API would reject with 400.
 */
export function parseMaxBitrateParam(value: string | null | undefined): number | null {
  const trimmed = value?.trim();
  if (!trimmed) return null;
  const parsed = Number(trimmed);
  return isValidMaxBitrate(parsed) ? parsed : null;
}

/**
 * Canonical URL form of a ceiling: an absent/blank/out-of-contract value is
 * `null` (no parameter), everything else is the decimal bps the API accepts.
 * The player's query keys and its cache predicates both use this form, so a
 * hand-written `maxBitrate=1e7` or `maxBitrate=%20100000%20` URL still matches
 * the "current variant" slot instead of being mistaken for another variant.
 */
export function canonicalMaxBitrateParam(value: string | null | undefined): string | null {
  const parsed = parseMaxBitrateParam(value);
  return parsed === null ? null : String(parsed);
}

/**
 * STRM sources are Direct Play only; the API ignores `maxBitrate` for them, so
 * the selector must not pretend the choice would change anything.
 */
export function supportsQualitySelection(source: MediaSource | undefined): boolean {
  return source?.sourceKind !== "STRM_URL";
}

function sourceBitrateOf(source: MediaSource | undefined): number | null {
  const bitrate = source?.bitrate;
  return typeof bitrate === "number" && Number.isFinite(bitrate) && bitrate > 0 ? bitrate : null;
}

/**
 * Ceilings to offer for `source`. A known source bitrate only shows ceilings that
 * are actually lower, because capping above the source can never change the
 * output. An unknown (unprobed) source bitrate shows every tier — the server does
 * not upgrade either, but the choice is preserved for when it is probed.
 *
 * A previously selected ceiling is always kept selectable, even when it is above
 * the current source bitrate, so switching versions cannot silently rewrite the
 * user's URL choice.
 */
export function qualityOptions(
  source: MediaSource | undefined,
  selectedMaxBitrate: number | null,
): QualityOption[] {
  const selected = selectedMaxBitrate !== null && isValidMaxBitrate(selectedMaxBitrate)
    ? selectedMaxBitrate
    : null;
  const automatic: QualityOption = {
    value: "",
    label: "原画（自动）",
    bitrate: null,
    isCurrent: selected === null,
  };
  if (!supportsQualitySelection(source)) return [automatic];
  const sourceBitrate = sourceBitrateOf(source);
  const tiers: number[] = QUALITY_TIER_BPS.filter(
    (bitrate) => sourceBitrate === null || bitrate < sourceBitrate,
  );
  if (selected !== null && !tiers.includes(selected)) {
    tiers.push(selected);
    tiers.sort((left, right) => right - left);
  }
  return [
    automatic,
    ...tiers.map((bitrate) => ({
      value: String(bitrate),
      label: qualityRateLabel(bitrate) ?? String(bitrate),
      bitrate,
      isCurrent: bitrate === selected,
    })),
  ];
}

/** A source with no ceiling below it has nothing to select besides "automatic". */
export function shouldShowQualitySelector(options: readonly QualityOption[]): boolean {
  return options.some((option) => option.bitrate !== null);
}

/** Position of the bitrate ceiling in the playback query keys. */
export function bitrateParamFromQueryKey(key: readonly unknown[]): string {
  const value = key[5];
  return typeof value === "string" ? value : "default";
}

/**
 * Returns a copy of the current URL parameters with the new ceiling. The
 * automatic choice is represented by an absent parameter, so a refresh keeps
 * whichever option the user picked without a second source of truth.
 */
export function qualitySwitchParams(
  current: URLSearchParams,
  maxBitrate: number | null,
): URLSearchParams {
  const next = new URLSearchParams(current);
  if (maxBitrate === null || !isValidMaxBitrate(maxBitrate)) next.delete("maxBitrate");
  else next.set("maxBitrate", String(maxBitrate));
  return next;
}

/**
 * What the current session is actually doing, from the response `bitrate`
 * summary: `targetBitrate` is the video bitrate the server encodes at, so a
 * non-null target is the only honest "limited" claim.
 */
export function qualityStatusText(
  bitrate: WebPlaybackBitrate | null | undefined,
  selectedMaxBitrate: number | null,
): string {
  // Without the response block the player knows nothing about the effective
  // output, not even that the ceiling was accepted: stay neutral instead of
  // claiming the source rate is below it.
  if (!bitrate) return "原画（自动）";
  const target = qualityRateLabel(bitrate.targetBitrate);
  if (bitrate.limited && target) return `已限制到 ${target}`;
  if (selectedMaxBitrate !== null) {
    // The server never upgrades an unprobed source, so the ceiling cannot have
    // been exceeded — but saying "not exceeded" would claim knowledge we lack.
    return bitrate.sourceBitrate === null
      ? "原画（源码率未知，未触发限制）"
      : "原画（源码率未超过所选上限）";
  }
  return "原画（自动）";
}

/**
 * URL parameters for switching the played version: the manual ceiling is a
 * user-wide preference and survives the change, while per-source track choices
 * (`audioStreamIndex`, `subtitleStreamIndex`) do not, because stream indexes
 * belong to the media source that is being replaced.
 */
export function sourceSwitchParams(
  sourceId: string,
  current: URLSearchParams,
): URLSearchParams {
  const next = new URLSearchParams({ sourceId });
  const maxBitrate = canonicalMaxBitrateParam(current.get("maxBitrate"));
  if (maxBitrate !== null) next.set("maxBitrate", maxBitrate);
  return next;
}
