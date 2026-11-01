import type { MediaSource, WebPlaybackResolution } from "../../lib/api/types";

/**
 * Pure model for the player's manual quality (resolution ceiling) selector.
 *
 * The backend accepts an optional `maxHeight` (pixels) on the playback session
 * and bootstrap requests and answers with a `resolution` summary describing what
 * it actually decided. Everything here is display/URL derivation only: it never
 * decides whether a source must be transcoded, it just proposes ceilings the
 * server is allowed to reject (and no ceiling above the source can change the
 * output).
 */

/** Same inclusive range the API validates (`400 INVALID_REQUEST` outside it). */
export const MAX_HEIGHT_MIN = 144;
export const MAX_HEIGHT_MAX = 4320;

/** Resolution tiers offered in the panel, highest first. */
export const QUALITY_RESOLUTION_TIERS = [2160, 1440, 1080, 720, 480, 360] as const;

export type QualityOption = {
  /**
   * Value written to the `maxHeight` URL parameter. The empty string is the
   * automatic choice, which is expressed by an absent parameter.
   */
  value: string;
  label: string;
  /** `null` for the automatic choice. */
  height: number | null;
  isCurrent: boolean;
};

/** Whole pixels; the 2160 tier is the familiar "4K" name, the rest read as `720p`. */
export function resolutionLabel(height: number): string {
  const rounded = Math.round(height);
  return rounded >= 2160 ? `${rounded}p（4K）` : `${rounded}p`;
}

export function isValidMaxHeight(value: number): boolean {
  return Number.isInteger(value) && value >= MAX_HEIGHT_MIN && value <= MAX_HEIGHT_MAX;
}

/**
 * Parses a `maxHeight` URL value. An absent, blank or out-of-contract value is
 * treated as "no ceiling" so a hand-edited URL can never make the player send a
 * request the API would reject with 400.
 */
export function parseMaxHeightParam(value: string | null | undefined): number | null {
  const trimmed = value?.trim();
  if (!trimmed) return null;
  const parsed = Number(trimmed);
  return isValidMaxHeight(parsed) ? parsed : null;
}

/**
 * Canonical URL form of a ceiling: an absent/blank/out-of-contract value is
 * `null` (no parameter), everything else is the decimal pixel height the API
 * accepts. The player's query keys and its cache predicates both use this form,
 * so a hand-written `maxHeight=1.08e3` or `maxHeight=%20720%20` URL still matches
 * the "current variant" slot instead of being mistaken for another variant.
 */
export function canonicalMaxHeightParam(value: string | null | undefined): string | null {
  const parsed = parseMaxHeightParam(value);
  return parsed === null ? null : String(parsed);
}

/**
 * STRM sources are Direct Play only; the API ignores `maxHeight` for them, so
 * the selector must not pretend the choice would change anything.
 */
export function supportsQualitySelection(source: MediaSource | undefined): boolean {
  return source?.sourceKind !== "STRM_URL";
}

/**
 * The video stream height ffprobe recorded for `source`, or `null` when the
 * probe never reported one. The Rust probe serializes ffprobe fields as-is
 * (`details.Height`); a lower-case spelling is accepted for older payloads.
 */
export function sourceVideoHeight(source: MediaSource | undefined): number | null {
  const video = source?.streams?.find((stream) => (stream.type ?? "").toUpperCase() === "VIDEO");
  const raw = video?.details?.Height ?? video?.details?.height;
  if (raw === undefined || raw === null || raw === "") return null;
  const height = typeof raw === "number" ? raw : Number(raw);
  return Number.isInteger(height) && height > 0 ? height : null;
}

function offeredTiers(sourceHeight: number | null): number[] {
  return QUALITY_RESOLUTION_TIERS.filter(
    (height) => sourceHeight === null || height < sourceHeight,
  );
}

/**
 * Tiers to offer for `source`. A known source height only shows tiers that are
 * actually lower, because capping at or above the source can never change the
 * output. An unknown (unprobed) source height shows every tier — the server does
 * not upgrade either, but the choice is preserved for when it is probed.
 *
 * A saved selection the current source cannot offer (a hand-written 900, or a
 * ceiling above a newly switched, smaller source) is kept as an option and
 * marked current. Dropping it would show 「原画（自动）」 while the requests keep
 * sending the ceiling, and the user would have no way to clear it.
 */
export function qualityOptions(
  source: MediaSource | undefined,
  selectedMaxHeight: number | null,
): QualityOption[] {
  const selected = selectedMaxHeight !== null && isValidMaxHeight(selectedMaxHeight)
    ? selectedMaxHeight
    : null;
  if (!supportsQualitySelection(source)) {
    return [{
      value: "",
      label: "原画（自动）",
      height: null,
      isCurrent: selected === null,
    }];
  }
  const tiers = offeredTiers(sourceVideoHeight(source));
  if (selected !== null && !tiers.includes(selected)) {
    tiers.push(selected);
    tiers.sort((left, right) => right - left);
  }
  return [
    {
      value: "",
      label: "原画（自动）",
      height: null,
      isCurrent: selected === null,
    },
    ...tiers.map((height) => ({
      value: String(height),
      label: resolutionLabel(height),
      height,
      isCurrent: height === selected,
    })),
  ];
}

/** A source with no lower tier has nothing to select besides "automatic". */
export function shouldShowQualitySelector(options: readonly QualityOption[]): boolean {
  return options.some((option) => option.height !== null);
}

/** Position of the resolution ceiling in the playback query keys. */
export function maxHeightParamFromQueryKey(key: readonly unknown[]): string {
  const value = key[5];
  return typeof value === "string" ? value : "default";
}

/**
 * Returns a copy of the current URL parameters with the new ceiling. The
 * automatic choice is represented by an absent parameter, so a refresh keeps
 * whichever option the user picked without a second source of truth. The legacy
 * `maxBitrate` parameter is not read or written.
 */
export function qualitySwitchParams(
  current: URLSearchParams,
  maxHeight: number | null,
): URLSearchParams {
  const next = new URLSearchParams(current);
  if (maxHeight === null || !isValidMaxHeight(maxHeight)) next.delete("maxHeight");
  else next.set("maxHeight", String(maxHeight));
  return next;
}

/**
 * What the current session is actually doing, from the response `resolution`
 * summary: a limited session reports the height it was scaled to, an unlimited
 * one reports the height the server really emits (`targetHeight`, which may
 * differ from the probed source height). Without the block (or without a
 * height) the player knows nothing about the effective output, so it stays
 * neutral instead of claiming the ceiling was accepted.
 */
export function qualityStatusText(
  resolution: WebPlaybackResolution | null | undefined,
): string {
  if (!resolution) return "原画（自动）";
  if (resolution.limited === null || resolution.limited === undefined) {
    // `limited` missing means the server did not describe the output at all.
    return "原画（自动）";
  }
  if (resolution.limited) {
    // The server says it scaled, so the target height is the only honest claim;
    // without it the player must not fall back to the source height.
    return resolution.targetHeight !== null
      ? `已缩放到 ${resolution.targetHeight}p`
      : "原画（自动）";
  }
  const height = resolution.targetHeight ?? resolution.sourceHeight;
  if (height === null) return "原画（自动）";
  return `原画 ${height}p`;
}

/**
 * URL parameters for switching the played version: the manual ceiling is a
 * user-wide preference and survives the change, while per-source track choices
 * (`audioStreamIndex`, `subtitleStreamIndex`) do not, because stream indexes
 * belong to the media source that is being replaced. The legacy `maxBitrate`
 * parameter is dropped with them because the player no longer reads it.
 */
export function sourceSwitchParams(
  sourceId: string,
  current: URLSearchParams,
): URLSearchParams {
  const next = new URLSearchParams({ sourceId });
  const maxHeight = canonicalMaxHeightParam(current.get("maxHeight"));
  if (maxHeight !== null) next.set("maxHeight", maxHeight);
  return next;
}
