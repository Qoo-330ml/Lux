import type { MediaSource } from "../../lib/api/types";
import { isHevcCodec } from "./media-codec";

const CLIENT_HEVC_CONTAINERS = new Set(["mp4", "m4v", "mov"]);

export function hasClientHevcCandidate(source: MediaSource | undefined) {
  if (!source || source.sourceKind === "STRM_URL" || !CLIENT_HEVC_CONTAINERS.has((source.container ?? "").toLowerCase())) return false;
  const video = source.streams?.find((stream) => (stream.type ?? "").toUpperCase() === "VIDEO");
  return isHevcCodec(video?.codec);
}

export function shouldUseClientHevc(source: MediaSource | undefined, video: HTMLVideoElement) {
  if (!hasClientHevcCandidate(source) || !hasClientHevcRuntime()) return false;
  const codec = source?.streams?.find((stream) => (stream.type ?? "").toUpperCase() === "VIDEO")?.codec;
  const codecHint = codec && /^(hvc1|hev1)\./i.test(codec) ? codec : "hvc1.1.6.L120.B0";
  return video.canPlayType(`video/mp4; codecs="${codecHint}"`) === "";
}

export function hasClientHevcRuntime() {
  const browserGlobals = globalThis as typeof globalThis & { VideoEncoder?: unknown };
  return typeof WebAssembly !== "undefined"
    && typeof Worker !== "undefined"
    && typeof MediaSource !== "undefined"
    && typeof browserGlobals.VideoEncoder === "function"
    && typeof MediaSource.isTypeSupported === "function"
    && MediaSource.isTypeSupported('video/mp4; codecs="avc1.640028"');
}
