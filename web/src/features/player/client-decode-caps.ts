import type { MediaStream } from "../../lib/api/types";
import { isHevcCodec } from "./media-codec";

/**
 * Media facts the client decode paths need before they promise realtime playback.
 *
 * The probe DTO (`MediaStream.details`) is the ffprobe metadata Lux already stores; the keys are
 * capitalized (`Width`, `Height`, `BitDepth`, `PixelFormat`, `Profile`) because the Rust probe
 * serializes ffprobe fields directly. Older/derived payloads sometimes use lower-case keys, so every
 * read below accepts both spellings instead of changing the backend DTO.
 *
 * This module stays pure: it only interprets the stream DTO, and callers still decide whether a
 * browser can actually decode the resulting codec strings.
 */
export type VideoCodecFamily = "h264" | "hevc" | "vp9" | "av1" | "other";

export type VideoStreamFacts = {
  /** Lower-case codec label as the API reported it (`h264`, `hevc`, `av1`, ...). */
  codec: string;
  family: VideoCodecFamily;
  width: number;
  height: number;
  /** 8 unless the probe metadata proves a higher depth. */
  bitDepth: number;
  profile: string | null;
  /** `details.Level` as ffprobe reports it (`153` for HEVC level 5.1), or null when unknown. */
  level: number | null;
};

/** Matches the historical assume-1080p behaviour when a probe carried no dimensions. */
export const DEFAULT_VIDEO_WIDTH = 1920;
export const DEFAULT_VIDEO_HEIGHT = 1080;

/**
 * WASM HEVC soft decoding (decode in WASM, re-encode to H.264 through WebCodecs) is CPU bound.
 * Recorded measurements on this project's test devices were `speedX ≈ 0.2–0.4` for a 4K source and
 * still below realtime at 2160p after the incremental-streaming fix, so 2160p can never be expected
 * to play in realtime. Anything above 1080p is therefore never sent to the client engine and the
 * session goes to server-side transcoding instead of stalling on “正在准备客户端解码…”.
 *
 * 1080p is also the upper bound Lux promises for client-side soft decoding (docs/LUX-DEVELOPMENT.md
 * 客户端解码增强 goals list 4K HEVC as a future target, not as a current commitment).
 */
export const WASM_SOFT_DECODE_MAX_WIDTH = 1920;
export const WASM_SOFT_DECODE_MAX_HEIGHT = 1080;

/** Byte values of the H.264 `profile_idc` nibble used in MSE codec strings. */
const H264_PROFILE_HIGH = "64";
const H264_PROFILE_HIGH_10 = "6E";

/** hvc1/HEVC Main 10 MSE strings emitted by the Matroska remux worker. */
const HEVC_MAIN10_MSE_CODECS = ["hvc1.2.4.L120.B0", "hvc1.2.4.L153.B0"] as const;
/** HEVC Main (8-bit) MSE string, kept for the pass-through remux path. */
const HEVC_MAIN_MSE_CODECS = ["hvc1.1.6.L120.B0"] as const;
/** HEVC `general_profile_idc` values used by MSE codec strings. */
const HEVC_PROFILE_MAIN = 1;
const HEVC_PROFILE_MAIN10 = 2;

const HIGH_BIT_DEPTH_PIXEL_FORMAT = /(?:yuv|yuva|yuvj|gbr|gbrap)\d{0,3}p?(\d{1,2})(?:le|be)?$|^p0?(\d{2})(?:le|be)?$/i;

export function codecFamily(codec: string): VideoCodecFamily {
  const normalized = codec.trim().toLowerCase();
  if (isHevcCodec(normalized)) return "hevc";
  if (/^(h264|avc|avc1|x264)(\.|$)/.test(normalized)) return "h264";
  if (/^(vp9|vp09)(\.|$)/.test(normalized)) return "vp9";
  if (/^(av1|av01)(\.|$)/.test(normalized)) return "av1";
  return "other";
}

/**
 * The MSE codec string describing the *source* stream. A 10-bit stream must be probed with its
 * 10-bit string, otherwise `MediaSource.isTypeSupported` answers for an 8-bit decoder the browser
 * may have, and the client remux then fails on the first `SourceBuffer.appendBuffer`.
 */
export function mseVideoCodecString(facts: VideoStreamFacts): string | null {
  switch (facts.family) {
    case "h264":
      return h264CodecForDimensions(facts.width, facts.height, facts.bitDepth);
    case "hevc":
      return hevcMseCodecCandidates(facts)[0];
    case "vp9":
      // vp09.<profile>.<level>.<bitDepth>: profile 2 is the 10-bit profile.
      return facts.bitDepth > 8 ? "vp09.02.10.10" : "vp09.00.10.08";
    case "av1":
      return facts.bitDepth > 8 ? "av01.0.04M.10" : "av01.0.04M.08";
    default:
      return null;
  }
}

/** H.264 level ladder: High@4.0 → High@4.2 → High@5.1, plus the High 10 profile for 10-bit. */
export function h264CodecForDimensions(width: number, height: number, bitDepth = 8) {
  const profile = bitDepth > 8 ? H264_PROFILE_HIGH_10 : H264_PROFILE_HIGH;
  const pixels = Math.max(1, width) * Math.max(1, height);
  if (pixels > 2_073_600) return `avc1.${profile}0033`;
  if (pixels > 921_600) return `avc1.${profile}002a`;
  return `avc1.${profile}0028`;
}

/**
 * Every HEVC MSE string worth probing for the source.
 *
 * The Matroska remux worker derives the codec string it will `addSourceBuffer` from the stream's own
 * `hvcC` box, so the capability probe must cover the same level (a 4K source is commonly level 5.0
 * (`L150`) or 5.1 (`L153`), which the historical hard-coded `L120`/`L153` list only partly covers).
 * The probe-aligned string therefore comes first and the historical list stays as a fallback.
 *
 * A 10-bit source must not be accepted because the browser happens to decode 8-bit Main, so only
 * Main 10 strings are offered there; 8-bit sources keep the historical list (Main 10 first) so the
 * Safari/MSE probe does not shrink.
 */
export function hevcMseCodecCandidates(facts: VideoStreamFacts): readonly string[] {
  const profileIdc = facts.bitDepth > 8 ? HEVC_PROFILE_MAIN10 : HEVC_PROFILE_MAIN;
  const probed = hevcCodecStringForLevel(profileIdc, facts.level);
  const standard = facts.bitDepth > 8
    ? HEVC_MAIN10_MSE_CODECS
    : [...HEVC_MAIN10_MSE_CODECS, ...HEVC_MAIN_MSE_CODECS];
  return probed === null ? standard : [...new Set([probed, ...standard])];
}

/** Mirrors `mkv-remux.hevcCodecString` for the probe details ffprobe already stored. */
function hevcCodecStringForLevel(profileIdc: number, level: number | null) {
  if (level === null || !Number.isFinite(level) || level <= 0) return null;
  const compatibility = profileIdc === HEVC_PROFILE_MAIN10 ? 4 : 6;
  return `hvc1.${profileIdc}.${compatibility}.L${Math.round(level)}.B0`;
}

/**
 * True when the WASM soft-decode path can be expected to keep up with the source in realtime.
 * Unknown dimensions keep the historical 1080p assumption instead of silently disabling the path.
 */
export function wasmSoftDecodeExpectsRealtime(facts: VideoStreamFacts) {
  return facts.width <= WASM_SOFT_DECODE_MAX_WIDTH && facts.height <= WASM_SOFT_DECODE_MAX_HEIGHT;
}

export function videoStreamFacts(stream: MediaStream | undefined): VideoStreamFacts {
  const codec = (stream?.codec ?? "").trim().toLowerCase();
  const details = stream?.details ?? {};
  const width = positiveInteger(detailNumber(details, ["Width", "width"]));
  const height = positiveInteger(detailNumber(details, ["Height", "height"]));
  return {
    codec,
    family: codecFamily(codec),
    width: width ?? DEFAULT_VIDEO_WIDTH,
    height: height ?? DEFAULT_VIDEO_HEIGHT,
    bitDepth: videoBitDepth(stream),
    profile: detailText(details, ["Profile", "profile"]),
    level: positiveInteger(detailNumber(details, ["Level", "level"])),
  };
}

/**
 * Bit depth of the video stream, inferred without any backend change:
 * 1. `BitDepth` (ffprobe `bits_per_raw_sample`), 2. `PixelFormat` (`yuv420p10le`, `p010le`, ...),
 * 3. the `Profile` label (`Main 10`, `High 10`, ...), 4. the HEVC profile nibble of a codec string.
 * Everything unknown stays 8-bit, which is the conservative default for MSE support checks.
 */
export function videoBitDepth(stream: MediaStream | undefined): number {
  const details = stream?.details ?? {};
  const explicit = detailNumber(details, ["BitDepth", "bit_depth"]);
  if (explicit !== null && explicit > 0) return Math.round(explicit);

  const pixelFormat = detailText(details, ["PixelFormat", "pixel_format", "pix_fmt"]);
  const pixelFormatDepth = pixelFormat ? bitDepthForPixelFormat(pixelFormat) : null;
  if (pixelFormatDepth !== null) return pixelFormatDepth;

  const profile = detailText(details, ["Profile", "profile"]) ?? "";
  const profileDepth = bitDepthForProfile(profile);
  if (profileDepth !== null) return profileDepth;

  const codec = (stream?.codec ?? "").trim().toLowerCase();
  // hvc1.<profile_idc>.<compatibility>...: profile 2 is Main 10, 3 Main Still, 4 RExt.
  const hevcProfile = codec.match(/^(?:hvc1|hev1)\.(\d)\./);
  if (hevcProfile && Number(hevcProfile[1]) >= 2) return 10;
  return 8;
}

function bitDepthForPixelFormat(pixelFormat: string): number | null {
  const match = pixelFormat.trim().match(HIGH_BIT_DEPTH_PIXEL_FORMAT);
  if (!match) return null;
  const depth = Number(match[1] ?? match[2]);
  return Number.isFinite(depth) && depth > 8 ? depth : null;
}

function bitDepthForProfile(profile: string): number | null {
  const normalized = profile.trim().toLowerCase();
  if (!normalized) return null;
  if (/main\s*12|12.?\s*bit/.test(normalized)) return 12;
  if (/main\s*10|main10|high\s*10|10.?\s*bit/.test(normalized)) return 10;
  return null;
}

function detailValue(details: Record<string, unknown>, keys: readonly string[]): unknown {
  for (const key of keys) {
    const value = details[key];
    if (value === undefined || value === null || value === "") continue;
    return value;
  }
  return undefined;
}

function detailNumber(details: Record<string, unknown>, keys: readonly string[]): number | null {
  const value = detailValue(details, keys);
  if (typeof value === "number") return Number.isFinite(value) ? value : null;
  if (typeof value === "string") {
    const parsed = Number(value.trim());
    return Number.isFinite(parsed) ? parsed : null;
  }
  return null;
}

function detailText(details: Record<string, unknown>, keys: readonly string[]): string | null {
  const value = detailValue(details, keys);
  return typeof value === "string" ? value.trim() || null : null;
}

function positiveInteger(value: number | null): number | null {
  if (value === null || !Number.isFinite(value) || value <= 0) return null;
  return Math.round(value);
}
