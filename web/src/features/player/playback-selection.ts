import type { MediaSource } from "../../lib/api/types";
import { isHevcCodec } from "./media-codec";

const CLIENT_HEVC_CONTAINERS = new Set(["mp4", "m4v", "mov"]);
const H264_CODECS = ["avc1.640028", "avc1.64002a", "avc1.640033"] as const;

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

export async function shouldUseClientHevc(source: MediaSource | undefined, video: HTMLVideoElement) {
  if (!hasClientHevcCandidate(source) || !hasClientHevcRuntime()) return false;
  const videoStream = source?.streams?.find((stream) => (stream.type ?? "").toUpperCase() === "VIDEO");
  const codec = videoStream?.codec;
  const codecHint = codec && /^(hvc1|hev1)\./i.test(codec) ? codec : "hvc1.1.6.L120.B0";
  if (video.canPlayType(`video/mp4; codecs="${codecHint}"`) !== "") return false;
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
