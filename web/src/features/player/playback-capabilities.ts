import type { MediaSource, WebPlaybackCapabilities } from "../../lib/api/types";
import { canUseHls } from "./hls-capabilities";

export type WebPlaybackCapabilityOptions = {
  /**
   * The runtime detected that the native element decoded no audio at all for an audio track the
   * browser claimed to support, so the retry must re-encode the audio track.
   */
  forceAudioTranscode?: boolean;
};

/**
 * Codecs a browser can *claim* to decode while it decodes no frame.
 *
 * Edge without the Dolby extension answers `canPlayType('audio/mp4; codecs="ac-3"')` and then plays
 * silence, and Chromium opens a Matroska whose video track is MPEG-4 Part 2 while decoding only the
 * audio. Once a source already played as Direct Play and misbehaved, the retry (`attempt > 0`) must
 * not copy these tracks again, so the server picks the audio/video transcode tier instead.
 */
const UNTRUSTWORTHY_COPY_CODECS = new Set(["ac3", "ac-3", "eac3", "ec-3", "vc1"]);

function isUntrustworthyCopyCodec(codec: string) {
  const normalized = codec.trim().toLowerCase();
  if (!normalized) return false;
  if (UNTRUSTWORTHY_COPY_CODECS.has(normalized)) return true;
  // MPEG-4 Part 2 (`mpeg4`, `mp4v.20.9`) and the Windows Media family (`msmpeg4v3`, `wmv3`).
  return /^(mpeg4|mp4v|msmpeg4|wmv)(\.|$|v?\d)/.test(normalized);
}

/**
 * Web capability probe for one playback attempt. Kept as a pure function so the
 * player page can derive both the bootstrap payload and the per-source session
 * payload from the same rules.
 */
export function webPlaybackCapabilities(
  source: MediaSource | undefined,
  attempt: number,
  videoOverride?: HTMLVideoElement | null,
  options: WebPlaybackCapabilityOptions = {},
): WebPlaybackCapabilities {
  const streams = source?.streams ?? [];
  const video = videoOverride ?? (typeof document === "undefined" ? null : document.createElement("video"));
  const videoCodec = (streams.find((stream) => stream.type?.toUpperCase() === "VIDEO")?.codec ?? "").toLowerCase();
  const audioCodec = (streams.find((stream) => stream.type?.toUpperCase() === "AUDIO")?.codec ?? "").toLowerCase();
  // A retry re-requests the same source after a Direct Play failure, so a claim the browser already
  // disproved (`ac-3`, `eac3`, `mpeg4`, `vc1`, ...) must not be offered as a copy again.
  const distrustClaims = attempt > 0;
  const videoCopyToFmp4 = !(distrustClaims && isUntrustworthyCopyCodec(videoCodec))
    && supportsMp4Codec(video, "video", videoCodec);
  const audioCopyToFmp4 = options.forceAudioTranscode !== true
    && !(distrustClaims && isUntrustworthyCopyCodec(audioCodec))
    && (!audioCodec || supportsMp4Codec(video, "audio", audioCodec));
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
