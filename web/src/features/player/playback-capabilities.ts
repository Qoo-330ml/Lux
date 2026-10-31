import type { MediaSource, WebPlaybackCapabilities } from "../../lib/api/types";
import { canUseHls } from "./hls-capabilities";

/**
 * Web capability probe for one playback attempt. Kept as a pure function so the
 * player page can derive both the bootstrap payload and the per-source session
 * payload from the same rules.
 */
export function webPlaybackCapabilities(
  source: MediaSource | undefined,
  attempt: number,
  videoOverride?: HTMLVideoElement | null,
): WebPlaybackCapabilities {
  const streams = source?.streams ?? [];
  const video = videoOverride ?? (typeof document === "undefined" ? null : document.createElement("video"));
  const videoCodec = (streams.find((stream) => stream.type?.toUpperCase() === "VIDEO")?.codec ?? "").toLowerCase();
  const audioCodec = (streams.find((stream) => stream.type?.toUpperCase() === "AUDIO")?.codec ?? "").toLowerCase();
  const videoCopyToFmp4 = supportsMp4Codec(video, "video", videoCodec);
  const audioCopyToFmp4 = !audioCodec || supportsMp4Codec(video, "audio", audioCodec);
  return {
    directPlay: attempt === 0,
    hls: canUseHls(video),
    videoCopyToFmp4,
    audioCopyToFmp4,
    hardwareTranscode: false,
    softwareTranscode: true,
  };
}

function supportsMp4Codec(
  video: HTMLVideoElement | null,
  kind: "audio" | "video",
  codec: string,
): boolean {
  if (!video || !codec) return false;
  const candidates = codecCandidates(kind, codec);
  return candidates.some((candidate) => {
    const mime = `${kind}/mp4; codecs="${candidate}"`;
    if (video.canPlayType(mime) !== "") return true;
    return typeof MediaSource !== "undefined"
      && typeof MediaSource.isTypeSupported === "function"
      && MediaSource.isTypeSupported(mime);
  });
}

function codecCandidates(kind: "audio" | "video", codec: string): string[] {
  const normalized = codec.toLowerCase();
  if (kind === "audio") {
    if (/^(aac|mp4a)(\.|$)/.test(normalized)) return [codec, "mp4a.40.2", "mp4a"];
    if (normalized === "ac3" || normalized === "ac-3") return [codec, "ac-3"];
    if (normalized === "eac3" || normalized === "ec-3") return [codec, "ec-3"];
    return [codec];
  }
  if (/^(h264|avc|avc1)(\.|$)/.test(normalized)) return [codec, "avc1"];
  if (/^(hevc|h265|hvc1|hev1)(\.|$)/.test(normalized)) return [codec, "hvc1"];
  if (/^(vp9|vp09)(\.|$)/.test(normalized)) return [codec, "vp09"];
  if (/^(av1|av01)(\.|$)/.test(normalized)) return [codec, "av01"];
  return [codec];
}
