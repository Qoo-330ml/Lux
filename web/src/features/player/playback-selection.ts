import type { MediaSource, MediaStream } from "../../lib/api/types";
import {
  codecFamily,
  DEFAULT_VIDEO_HEIGHT,
  DEFAULT_VIDEO_WIDTH,
  hevcMseCodecCandidates,
  mseVideoCodecString,
  videoStreamFacts,
  wasmSoftDecodeExpectsRealtime,
  type VideoStreamFacts,
} from "./client-decode-caps";
import { isHevcCodec } from "./media-codec";

export { h264CodecForDimensions } from "./client-decode-caps";

const CLIENT_HEVC_CONTAINERS = new Set(["mp4", "m4v", "mov"]);
const H264_CODECS = ["avc1.640028", "avc1.64002a", "avc1.640033"] as const;
const PASSTHROUGH_AUDIO_CODECS = new Set(["ac3", "ac-3", "eac3", "ec-3", "opus"]);
const CLIENT_MKV_VIDEO_CODECS = new Set(["h264", "avc", "avc1", "hevc", "h265", "hvc1", "vp9", "vp09", "av1", "av01"]);

/** Facts used when a caller only needs the historical "could this ever work" answer. */
const DEFAULT_HEVC_FACTS: VideoStreamFacts = {
  codec: "hevc",
  family: "hevc",
  width: DEFAULT_VIDEO_WIDTH,
  height: DEFAULT_VIDEO_HEIGHT,
  bitDepth: 8,
  profile: null,
  level: null,
};

/** Why the client decode path was not taken. */
export type ClientDecodeRejection =
  /** The source never had a client engine candidate; ordinary playback decides. */
  | "no-candidate"
  /** The element plays the original container/codec itself, so no client engine is needed. */
  | "native-playback"
  /**
   * The container is one the element *claims* to open, but the video codec is outside the
   * native whitelist (MPEG-4 Part 2, VC-1, ...), so its `canPlayType` answer cannot be trusted.
   */
  | "native-codec-unsupported"
  /** The browser cannot decode the source's real bit depth/level through MSE or WebCodecs. */
  | "client-codec-unsupported"
  /** Only the CPU-bound WASM soft decoder is left, and the source is above its realtime cap. */
  | "client-resolution-too-high"
  /** The required WASM/Worker/MSE/WebCodecs runtime is missing. */
  | "client-runtime-missing";

export type ClientDecodeDecision =
  | { engine: "client-mkv" | "client-hevc"; rejected: null }
  | { engine: null; rejected: ClientDecodeRejection };

const REJECTED = (rejected: ClientDecodeRejection): ClientDecodeDecision => ({ engine: null, rejected });
const CLIENT_MKV: ClientDecodeDecision = { engine: "client-mkv", rejected: null };
const CLIENT_HEVC: ClientDecodeDecision = { engine: "client-hevc", rejected: null };

/**
 * True when the client path was required but cannot work in this browser, so the session must be
 * re-requested as server-side transcoding instead of waiting for a media element that never plays.
 */
export function clientDecodeRequiresServerFallback(decision: ClientDecodeDecision) {
  return decision.engine === null
    && (
      decision.rejected === "native-codec-unsupported"
      || decision.rejected === "client-codec-unsupported"
      || decision.rejected === "client-resolution-too-high"
      || decision.rejected === "client-runtime-missing"
    );
}

export function isRemoteHttpStrmSource(source: MediaSource | undefined) {
  if (source?.sourceKind !== "STRM_URL" || !source.externalUrl) return false;
  try {
    const url = new URL(source.externalUrl);
    return url.protocol === "http:" || url.protocol === "https:";
  } catch {
    return false;
  }
}

export function hasClientHevcCandidate(source: MediaSource | undefined) {
  if (!source || (source.sourceKind === "STRM_URL" && !source.externalUrl) || !CLIENT_HEVC_CONTAINERS.has((source.container ?? "").toLowerCase())) return false;
  const video = source.streams?.find((stream) => (stream.type ?? "").toUpperCase() === "VIDEO");
  return isHevcCodec(video?.codec);
}

export function hasClientMkvCandidate(source: MediaSource | undefined) {
  if (!source || (source.sourceKind === "STRM_URL" && !source.externalUrl) || !isMatroskaContainer(source.container)) return false;
  const video = source.streams?.find((stream) => (stream.type ?? "").toUpperCase() === "VIDEO");
  if (!video?.codec || !CLIENT_MKV_VIDEO_CODECS.has(video.codec.toLowerCase().trim()) && !isHevcCodec(video.codec)) return false;
  const audio = source.streams?.filter((stream) => (stream.type ?? "").toUpperCase() === "AUDIO") ?? [];
  return audio.length === 0 || audio.some((stream) => /^aac$|^mp4a\./i.test(stream.codec ?? "") || PASSTHROUGH_AUDIO_CODECS.has((stream.codec ?? "").toLowerCase()));
}

function isMatroskaContainer(container: string | null | undefined) {
  return (container ?? "")
    .toLowerCase()
    .split(",")
    .some((part) => ["mkv", "matroska", "webm"].includes(part.trim()));
}

/**
 * The video codecs whose native Matroska playback Lux trusts the element's own answer for.
 *
 * A browser answers `canPlayType('video/x-matroska; codecs="…"')` for the *container plus one
 * codec string*, and Chromium answers “maybe” for a Matroska whose video track is MPEG-4 Part 2 and
 * then decodes only the audio track. Native Matroska playback is therefore limited to the codecs the
 * player can also gate through the remux path; anything else is handed to server transcoding.
 */
export function isNativeMkvVideoCodec(codec: string | null | undefined) {
  const normalized = (codec ?? "").trim().toLowerCase();
  return normalized !== "" && codecFamily(normalized) !== "other";
}

function hasNativeMkvVideoCodec(source: MediaSource) {
  return isNativeMkvVideoCodec(videoStreamOf(source)?.codec);
}

/**
 * A local Matroska whose video codec is outside the native whitelist must not be played by the
 * element: the source is re-requested as server transcoding. Remote `.strm` sources stay exempt —
 * they have no server plan and can only play whatever the element manages.
 */
function refusesNativeMatroska(codecHolder: MediaSource) {
  return isMatroskaContainer(codecHolder.container)
    && !isRemoteHttpStrmSource(codecHolder)
    && !hasNativeMkvVideoCodec(codecHolder);
}

function videoStreamOf(source: MediaSource | undefined): MediaStream | undefined {
  return source?.streams?.find((stream) => (stream.type ?? "").toUpperCase() === "VIDEO");
}

export type ClientMkvSelectionOptions = {
  /**
   * A native Matroska path is enough for ordinary playback, but it does not
   * expose embedded text tracks consistently. Caption playback therefore
   * must keep the client demux/remux path even when the video element reports
   * that it can play Matroska directly.
   */
  requireCaptionPipeline?: boolean;
};

export async function shouldUseClientHevc(source: MediaSource | undefined, video: HTMLVideoElement) {
  return (await resolveClientDecode(source, video)).engine === "client-hevc";
}

export async function shouldUseClientMkv(
  source: MediaSource | undefined,
  video: HTMLVideoElement,
  options: ClientMkvSelectionOptions = {},
) {
  return (await resolveClientDecode(source, video, options)).engine === "client-mkv";
}

/**
 * Decides whether the client decode path should be used and, when it should not, why.
 *
 * The runtime needs the reason: `native-playback` and `no-candidate` keep ordinary playback, while
 * the `client-*` rejections mean the browser cannot decode what the server planned to send and the
 * session has to be re-requested as server-side transcoding.
 */
export async function resolveClientDecode(
  source: MediaSource | undefined,
  video: HTMLVideoElement,
  options: ClientMkvSelectionOptions = {},
): Promise<ClientDecodeDecision> {
  if (!source) return REJECTED("no-candidate");
  const mkvCandidate = hasClientMkvCandidate(source);
  const hevcCandidate = hasClientHevcCandidate(source);
  if (!mkvCandidate && !hevcCandidate) {
    // A Matroska whose video codec Lux does not gate natively (MPEG-4 Part 2, VC-1, ...) must not
    // fall through to the element: Chromium accepts the container and plays only the audio track.
    return REJECTED(refusesNativeMatroska(source) ? "native-codec-unsupported" : "no-candidate");
  }

  const facts = videoStreamFacts(videoStreamOf(source));
  const requireCaptionPipeline = options.requireCaptionPipeline === true;

  if (mkvCandidate) {
    return decideMatroska(source, facts, video, requireCaptionPipeline);
  }
  return decideMp4Hevc(source, facts, video);
}

async function decideMatroska(
  source: MediaSource,
  facts: VideoStreamFacts,
  video: HTMLVideoElement,
  requireCaptionPipeline: boolean,
): Promise<ClientDecodeDecision> {
  // A 10-bit source must be hinted with its real profile: `codecs="avc1"` describes the 8-bit
  // sibling the browser may support, which is how a High 10 file used to reach the remux worker.
  const codecHint = matroskaCodecHint(facts);
  const nativeMatroska = codecHint !== null
    && video.canPlayType(`video/x-matroska; codecs="${codecHint}"`) !== "";
  if (!requireCaptionPipeline && nativeMatroska) return REJECTED("native-playback");

  if (facts.family !== "hevc") {
    // H.264/VP9/AV1 Matroska is remuxed without re-encoding, so MSE must accept the source's own
    // profile and bit depth.
    return decideMatroskaRemux(source, facts);
  }

  if (hasClientMkvHevcRuntime(facts)) {
    // The browser decodes HEVC itself, so the worker only rewraps Matroska into fMP4. This is the
    // Safari/Windows path and it is not bounded by the WASM soft-decode cap.
    if (!hasClientMkvAudioRuntime(source, facts)) return REJECTED("client-codec-unsupported");
    return requireCaptionPipeline || !nativeMatroska ? CLIENT_MKV : REJECTED("native-playback");
  }

  // No HEVC support in MSE: only the CPU-bound WASM decoder + H.264 encoder is left.
  if (!wasmSoftDecodeExpectsRealtime(facts)) return REJECTED("client-resolution-too-high");
  if (!hasClientHevcRuntime()) return REJECTED("client-runtime-missing");
  if (!hasClientMkvH264Audio(source)) return REJECTED("client-codec-unsupported");
  if (!requireCaptionPipeline && video.canPlayType('video/x-matroska; codecs="hvc1"') !== "") {
    return REJECTED("native-playback");
  }
  if (!(await probeWasmSoftDecode(source, facts))) return REJECTED("client-codec-unsupported");
  return CLIENT_MKV;
}

async function decideMp4Hevc(
  source: MediaSource,
  facts: VideoStreamFacts,
  video: HTMLVideoElement,
): Promise<ClientDecodeDecision> {
  const codecHint = facts.codec && /^(hvc1|hev1)\./i.test(facts.codec) ? facts.codec : "hvc1.1.6.L120.B0";
  if (video.canPlayType(`video/mp4; codecs="${codecHint}"`) !== "") return REJECTED("native-playback");
  // MP4/MOV HEVC always needs the WASM decoder + H.264 re-encode.
  if (!wasmSoftDecodeExpectsRealtime(facts)) return REJECTED("client-resolution-too-high");
  if (!hasClientHevcRuntime()) return REJECTED("client-runtime-missing");
  if (!(await probeWasmSoftDecode(source, facts))) return REJECTED("client-codec-unsupported");
  return CLIENT_HEVC;
}

function decideMatroskaRemux(source: MediaSource, facts: VideoStreamFacts): ClientDecodeDecision {
  if (typeof MediaSource === "undefined" || typeof MediaSource.isTypeSupported !== "function") {
    return REJECTED("client-runtime-missing");
  }
  const videoCodec = mseVideoCodecString(facts);
  if (!videoCodec) return REJECTED("client-codec-unsupported");
  const audioCodec = mseAudioCodec(source);
  const hasAudio = (source.streams ?? []).some((stream) => (stream.type ?? "").toUpperCase() === "AUDIO");
  if (hasAudio && !audioCodec) return REJECTED("client-codec-unsupported");
  return MediaSource.isTypeSupported(`video/mp4; codecs="${[videoCodec, audioCodec].filter(Boolean).join(",")}"`)
    ? CLIENT_MKV
    : REJECTED("client-codec-unsupported");
}

/**
 * The MSE codec string the native Matroska probe asks about, or `null` when the source's codec has
 * no trusted string. It must never fall back to `"avc1"`: that generic hint is what let a Matroska
 * with an undecodable video track be treated as natively playable (`nativeMatroska` above).
 */
function matroskaCodecHint(facts: VideoStreamFacts): string | null {
  return mseVideoCodecString(facts);
}

function mseAudioCodec(source: MediaSource) {
  const codecs = source.streams?.filter((stream) => (stream.type ?? "").toUpperCase() === "AUDIO").map((stream) => (stream.codec ?? "").toLowerCase()) ?? [];
  if (codecs.some((codec) => /^aac$|^mp4a\./i.test(codec))) return "mp4a.40.2";
  if (codecs.some((codec) => ["ac3", "ac-3"].includes(codec)) && mseAudioSupported("ac-3")) return "ac-3";
  if (codecs.some((codec) => ["eac3", "ec-3"].includes(codec)) && mseAudioSupported("ec-3")) return "ec-3";
  if (codecs.some((codec) => codec === "opus")) return "opus";
  return null;
}

/**
 * AC-3/E-AC-3 decoding is a platform feature (Windows needs the Dolby extension), and a browser can
 * accept the MSE pair while never decoding a frame of it. Requiring the audio-only answer keeps a
 * silent client remux from being preferred over the server's audio transcode.
 */
function mseAudioSupported(codec: "ac-3" | "ec-3" | "opus") {
  if (typeof MediaSource === "undefined" || typeof MediaSource.isTypeSupported !== "function") return false;
  return MediaSource.isTypeSupported(`audio/mp4; codecs="${codec}"`);
}

function hasClientMkvAudioRuntime(source: MediaSource, facts: VideoStreamFacts = DEFAULT_HEVC_FACTS) {
  const audio = source.streams?.filter((stream) => (stream.type ?? "").toUpperCase() === "AUDIO") ?? [];
  const codec = audio.some((stream) => /^aac$|^mp4a\./i.test(stream.codec ?? ""))
    ? "aac"
    : audio.some((stream) => ["ac3", "ac-3"].includes((stream.codec ?? "").toLowerCase()))
      ? "ac-3"
        : audio.some((stream) => ["eac3", "ec-3"].includes((stream.codec ?? "").toLowerCase()))
          ? "ec-3"
          : audio.some((stream) => (stream.codec ?? "").toLowerCase() === "opus")
            ? "opus"
        : null;
  if (!codec) return true;
  if (codec !== "aac" && !mseAudioSupported(codec)) return false;
  if (codec === "aac") return true;
  return hevcMseCodecCandidates(facts).some((videoCodec) => MediaSource.isTypeSupported(`video/mp4; codecs="${videoCodec},${codec}"`));
}

function hasClientMkvH264Audio(source: MediaSource) {
  const audio = source.streams?.filter((stream) => (stream.type ?? "").toUpperCase() === "AUDIO") ?? [];
  return audio.length === 0 || audio.some((stream) => /^aac$|^mp4a\./i.test(stream.codec ?? ""));
}

/** True when the browser can decode HEVC through MSE, which makes the worker a plain remux. */
export function hasClientMkvHevcRuntime(facts: VideoStreamFacts = DEFAULT_HEVC_FACTS) {
  if (typeof MediaSource === "undefined" || typeof MediaSource.isTypeSupported !== "function") return false;
  return hevcMseCodecCandidates(facts).some((codec) => MediaSource.isTypeSupported(`video/mp4; codecs="${codec}"`));
}

/**
 * Synchronous gate used by the player before it changes engines. It covers
 * the codec checks that do not require an async WebCodecs probe, so a remote
 * caption selection can leave native playback untouched when the MSE
 * combination is impossible (for example HEVC + E-AC-3 in Chrome).
 */
export function canUseClientMkvCaptionPipeline(source: MediaSource | undefined) {
  if (!source || !hasClientMkvCandidate(source)) return false;
  const facts = videoStreamFacts(videoStreamOf(source));
  if (facts.family !== "hevc") return decideMatroskaRemux(source, facts).engine === "client-mkv";
  if (hasClientMkvHevcRuntime(facts)) return hasClientMkvAudioRuntime(source, facts);
  if (!wasmSoftDecodeExpectsRealtime(facts)) return false;
  return hasClientMkvH264Audio(source);
}

/**
 * Remote Matroska captions can be read without remuxing the media.  Keep this
 * capability independent from the MSE codec pair: native playback remains in
 * charge of audio/video (including E-AC-3), while the caption sidecar only
 * needs a CORS/Range-capable remote source and a supported text track.
 */
export function canUseRemoteMkvCaptionSidecar(source: MediaSource | undefined) {
  if (!source || !isRemoteHttpStrmSource(source) || !isMatroskaContainer(source.container)) return false;
  return (source.streams ?? []).some((stream) => {
    if (stream.isExternal === true || (stream.type ?? "").toUpperCase() !== "SUBTITLE") return false;
    const codec = (stream.codec ?? "").trim().toLowerCase();
    return codec === "srt" || codec === "subrip" || codec === "ass" || codec === "ssa";
  });
}

/**
 * The WASM soft decoder re-encodes to H.264 through WebCodecs, so the probe asks the encoder for the
 * source's real resolution, framerate, bitrate and H.264 level.
 */
async function probeWasmSoftDecode(source: MediaSource, facts: VideoStreamFacts) {
  const browserGlobals = globalThis as typeof globalThis & {
    VideoEncoder?: {
      isConfigSupported: (config: Record<string, unknown>) => Promise<{ supported?: boolean }>;
    };
  };
  const videoStream = videoStreamOf(source);
  const framerate = probeFramerate(videoStream);
  try {
    const support = await browserGlobals.VideoEncoder?.isConfigSupported({
      codec: h264ProbeCodecCode(facts),
      width: facts.width,
      height: facts.height,
      bitrate: Math.max(1_000_000, source?.bitrate ?? 8_000_000),
      framerate,
    });
    return support?.supported === true;
  } catch {
    return false;
  }
}

function h264ProbeCodecCode(facts: VideoStreamFacts) {
  // The encoder always produces 8-bit H.264 (High@L4.0–5.1), never a 10-bit profile.
  const codec = mseVideoCodecString({ ...facts, family: "h264", bitDepth: 8 });
  return codec ?? "avc1.640028";
}

function probeFramerate(stream: MediaStream | undefined) {
  const details = stream?.details ?? {};
  const value = details.AverageFrameRate ?? details.averageFrameRate ?? details.RealFrameRate ?? details.realFrameRate;
  const parsed = typeof value === "number" ? value : typeof value === "string" ? Number(value) : Number.NaN;
  return Number.isFinite(parsed) && parsed > 0 ? parsed : 30;
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
