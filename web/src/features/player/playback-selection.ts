import type { MediaSource } from "../../lib/api/types";
import { isHevcCodec } from "./media-codec";

const CLIENT_HEVC_CONTAINERS = new Set(["mp4", "m4v", "mov"]);
const CLIENT_MKV_CONTAINERS = new Set(["mkv"]);
const H264_CODECS = ["avc1.640028", "avc1.64002a", "avc1.640033"] as const;
const HEVC_MSE_CODECS = ["hvc1.2.4.L153.B0", "hvc1.1.6.L120.B0"] as const;
const AC3_CODECS = new Set(["ac3", "ac-3"]);

export function h264CodecForDimensions(width: number, height: number) {
  const pixels = Math.max(1, width) * Math.max(1, height);
  if (pixels > 2_073_600) return "avc1.640033";
  if (pixels > 921_600) return "avc1.64002a";
  return "avc1.640028";
}

export function hasClientHevcCandidate(source: MediaSource | undefined) {
  if (!source || (source.sourceKind === "STRM_URL" && !source.externalUrl) || !CLIENT_HEVC_CONTAINERS.has((source.container ?? "").toLowerCase())) return false;
  const video = source.streams?.find((stream) => (stream.type ?? "").toUpperCase() === "VIDEO");
  return isHevcCodec(video?.codec);
}

export function hasClientMkvCandidate(source: MediaSource | undefined) {
  if (!source || (source.sourceKind === "STRM_URL" && !source.externalUrl) || !CLIENT_MKV_CONTAINERS.has((source.container ?? "").toLowerCase())) return false;
  const video = source.streams?.find((stream) => (stream.type ?? "").toUpperCase() === "VIDEO");
  if (!isHevcCodec(video?.codec)) return false;
  const audio = source.streams?.find((stream) => (stream.type ?? "").toUpperCase() === "AUDIO");
  return !audio?.codec || /^aac$|^mp4a\./i.test(audio.codec) || AC3_CODECS.has(audio.codec.toLowerCase());
}

export async function shouldUseClientHevc(source: MediaSource | undefined, video: HTMLVideoElement) {
  if (!source || !hasClientHevcCandidate(source) || !hasClientHevcRuntime()) return false;
  return probeClientHevc(source, video, "video/mp4");
}

export async function shouldUseClientMkv(source: MediaSource | undefined, video: HTMLVideoElement) {
  if (!source || !hasClientMkvCandidate(source)) return false;
  if (hasClientMkvHevcRuntime()) {
    if (!hasClientMkvAudioRuntime(source)) return false;
    return video.canPlayType('video/x-matroska; codecs="hvc1"') === "";
  }
  if (!hasClientHevcRuntime()) return false;
  return probeClientHevc(source, video, "video/x-matroska");
}

function hasClientMkvAudioRuntime(source: MediaSource) {
  const audio = source.streams?.find((stream) => (stream.type ?? "").toUpperCase() === "AUDIO");
  if (!audio?.codec || !AC3_CODECS.has(audio.codec.toLowerCase())) return true;
  return HEVC_MSE_CODECS.some((videoCodec) => MediaSource.isTypeSupported(`video/mp4; codecs="${videoCodec},ac-3"`));
}

export function hasClientMkvHevcRuntime() {
  if (typeof MediaSource === "undefined" || typeof MediaSource.isTypeSupported !== "function") return false;
  return HEVC_MSE_CODECS.some((codec) => MediaSource.isTypeSupported(`video/mp4; codecs="${codec}"`));
}

async function probeClientHevc(source: MediaSource, video: HTMLVideoElement, mime: string) {
  const videoStream = source?.streams?.find((stream) => (stream.type ?? "").toUpperCase() === "VIDEO");
  const codec = videoStream?.codec;
  const codecHint = codec && /^(hvc1|hev1)\./i.test(codec) ? codec : "hvc1.1.6.L120.B0";
  if (video.canPlayType(`${mime}; codecs="${codecHint}"`) !== "") return false;
  const browserGlobals = globalThis as typeof globalThis & {
    VideoEncoder?: {
      isConfigSupported: (config: Record<string, unknown>) => Promise<{ supported?: boolean }>;
    };
  };
  const details = videoStream?.details ?? {};
  const width = typeof details.width === "number" && Number.isFinite(details.width) && details.width > 0 ? Math.round(details.width) : 1920;
  const height = typeof details.height === "number" && Number.isFinite(details.height) && details.height > 0 ? Math.round(details.height) : 1080;
  const framerate = typeof details.averageFrameRate === "number" && Number.isFinite(details.averageFrameRate) && details.averageFrameRate > 0 ? details.averageFrameRate : 30;
  try {
    const support = await browserGlobals.VideoEncoder?.isConfigSupported({
      codec: h264CodecForDimensions(width, height),
      width,
      height,
      bitrate: Math.max(1_000_000, source?.bitrate ?? 8_000_000),
      framerate,
    });
    return support?.supported === true;
  } catch {
    return false;
  }
}

export function hasClientHevcRuntime() {
  const browserGlobals = globalThis as typeof globalThis & { VideoEncoder?: { isConfigSupported?: unknown } };
  return typeof WebAssembly !== "undefined"
    && typeof Worker !== "undefined"
    && typeof MediaSource !== "undefined"
    && typeof browserGlobals.VideoEncoder?.isConfigSupported === "function"
    && typeof MediaSource.isTypeSupported === "function"
    && H264_CODECS.some((codec) => MediaSource.isTypeSupported(`video/mp4; codecs="${codec}"`));
}
