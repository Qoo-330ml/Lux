/**
 * Watchdog for the client decode engines (`ClientMkvEngine` / `ClientHevcEngine`).
 *
 * Both engines wait on browser-owned steps that have no timeout of their own: the module Worker
 * becoming ready, the Matroska demuxer emitting its first `segment` message, and the MSE
 * `SourceBuffer` firing `updateend`. A Safari worker that never reports back, a Range request that
 * never settles or a `SourceBuffer` that silently drops an append therefore leaves
 * `ClientMkvEngine.setSource()` pending forever, and the player stays on “正在准备客户端解码…”
 * without ever reaching the server transcoding fallback.
 *
 * This module is the pure decision half of that watchdog: the runtime feeds it timestamps and
 * measured throughput, and the runtime turns the verdict into `requestServerFallback`.
 */

/** A client engine that produced no playable segment inside this window is treated as stuck. */
export const CLIENT_FIRST_SEGMENT_TIMEOUT_MS = 12_000;

/**
 * A native Direct Play element that never reached `HAVE_CURRENT_DATA` inside this window is stuck
 * too: Chromium happily opens a Matroska file whose video codec it cannot decode, and neither
 * `error` nor `loadedmetadata` follows. The window is longer than the client one because the element
 * still has to fetch real media bytes from the origin.
 */
export const DIRECT_FIRST_FRAME_TIMEOUT_MS = 20_000;

/** Below this measured speed the client path is not realtime; it only keeps the existing banner. */
export const CLIENT_DECODE_SLOW_SPEED_X = 0.8;

/**
 * Only clearly-too-slow playback is worth an automatic server fallback. The player already shows a
 * non-blocking “客户端解码速度低于实时” banner for anything under realtime, and viewers who keep
 * playing a cached-but-slow client decode must not be pushed onto the server; `0.5×` is the point
 * where the buffer can no longer keep up with a normal playback rate at all.
 */
export const CLIENT_DECODE_FALLBACK_SPEED_X = 0.5;

/** Consecutive samples required before a measured speed counts as sustained. */
export const CLIENT_DECODE_SLOW_SAMPLE_COUNT = 3;

export type ClientDecodeWatchdogFallbackReason = "stalled" | "slow";

export type ClientDecodeWatchdog = {
  /** When the client engine was created and `setSource` started. */
  startedAtMs: number;
  /** When the first playable segment reached the `SourceBuffer`, null while still waiting. */
  firstSegmentAtMs: number | null;
  /** Consecutive samples below `CLIENT_DECODE_SLOW_SPEED_X`. */
  slowSamples: number;
  /** Consecutive samples below `CLIENT_DECODE_FALLBACK_SPEED_X`. */
  fallbackSpeedSamples: number;
  lastSpeedX: number | null;
};

export type ClientDecodeWatchdogVerdict =
  /** Keep playing (and keep waiting for the first segment). */
  | { action: "continue" }
  /** Slower than realtime but still playable: only the existing banner applies. */
  | { action: "slow" }
  /** The client path cannot work; the runtime asks for a server transcoding session. */
  | { action: "fallback"; reason: ClientDecodeWatchdogFallbackReason };

export function startClientDecodeWatchdog(startedAtMs: number): ClientDecodeWatchdog {
  return {
    startedAtMs,
    firstSegmentAtMs: null,
    slowSamples: 0,
    fallbackSpeedSamples: 0,
    lastSpeedX: null,
  };
}

/** Records the first playable segment; the first-segment timeout no longer applies afterwards. */
export function markClientDecodeFirstSegment(
  watchdog: ClientDecodeWatchdog,
  nowMs: number,
): ClientDecodeWatchdog {
  if (watchdog.firstSegmentAtMs !== null) return watchdog;
  return { ...watchdog, firstSegmentAtMs: Math.max(watchdog.startedAtMs, nowMs) };
}

export function recordClientDecodeSpeedSample(
  watchdog: ClientDecodeWatchdog,
  speedX: number,
): ClientDecodeWatchdog {
  if (!Number.isFinite(speedX) || speedX < 0) return watchdog;
  return {
    ...watchdog,
    lastSpeedX: speedX,
    slowSamples: speedX < CLIENT_DECODE_SLOW_SPEED_X ? watchdog.slowSamples + 1 : 0,
    fallbackSpeedSamples: speedX < CLIENT_DECODE_FALLBACK_SPEED_X ? watchdog.fallbackSpeedSamples + 1 : 0,
  };
}

/** True when a native Direct Play element produced no decodable frame inside its window. */
export function evaluateDirectPlayWatchdog(watchdog: ClientDecodeWatchdog, nowMs: number): boolean {
  return watchdog.firstSegmentAtMs === null && nowMs - watchdog.startedAtMs >= DIRECT_FIRST_FRAME_TIMEOUT_MS;
}

/**
 * Playback time after which a native Direct Play element that still reports `videoWidth === 0` is
 * treated as “the browser opened the container but decodes no video frame”.
 *
 * Confirmed on macOS Chrome and Win11 Edge with a Matroska whose video track is MPEG-4 Part 2: the
 * element advanced `currentTime`, reached `readyState 4` and played the MP3 track while
 * `videoWidth`/`videoHeight` stayed 0, with no `error` and no notice anywhere.
 */
export const NATIVE_SILENT_VIDEO_MS = 2_000;

/** Playback time after which a native element that decoded no audio at all is treated as silent. */
export const NATIVE_SILENT_AUDIO_MS = 3_000;

/** How often the runtime samples the native element for the two silent-failure checks above. */
export const NATIVE_SILENT_POLL_MS = 500;

export type NativeSilentFailure =
  /** The source has a video track but the element never produced a frame. */
  | "silent-video"
  /** The source has an audio track the element claims to support but never decodes. */
  | "silent-audio";

/**
 * Accumulated *real* playback time for one native element.
 *
 * The watchdog must not fire while the element is buffering: a source that has not delivered bytes
 * yet also reports `videoWidth === 0`. Only media time that advanced while the element was playing
 * counts, and a single sample is capped so a background/suspended tab cannot fast-forward the clock.
 */
export type NativeSilentPlaybackClock = {
  playedMs: number;
  lastSampleMs: number | null;
  lastCurrentTime: number | null;
};

export function startNativeSilentPlaybackClock(): NativeSilentPlaybackClock {
  return { playedMs: 0, lastSampleMs: null, lastCurrentTime: null };
}

export function sampleNativeSilentPlayback(
  clock: NativeSilentPlaybackClock,
  sample: {
    nowMs: number;
    currentTime: number;
    paused: boolean;
    seeking: boolean;
  },
): NativeSilentPlaybackClock {
  if (!Number.isFinite(sample.currentTime)) return clock;
  const base = { lastSampleMs: sample.nowMs, lastCurrentTime: sample.currentTime };
  if (clock.lastSampleMs === null || clock.lastCurrentTime === null) {
    return { ...clock, ...base };
  }
  const elapsedMs = Math.min(sample.nowMs - clock.lastSampleMs, NATIVE_SILENT_POLL_MS * 2);
  const advanced = sample.currentTime > clock.lastCurrentTime;
  if (sample.paused || sample.seeking || !advanced) return { ...clock, ...base };
  return { playedMs: clock.playedMs + Math.max(0, elapsedMs), ...base };
}

export type NativeSilentFailureInput = {
  /** The source really carries a video track (cover art is already dropped by the probe). */
  hasVideoStream: boolean;
  hasAudioStream: boolean;
  /** Accumulated playback time from `sampleNativeSilentPlayback`, after `loadedmetadata`. */
  playedMs: number;
  /** `HTMLVideoElement.videoWidth`; 0 means the element decoded no video frame. */
  videoWidth: number | null | undefined;
  /** Chromium's `webkitAudioDecodedByteCount`; undefined/null when the browser exposes none. */
  audioDecodedBytes: number | null | undefined;
  /** Safari's `video.audioTracks.length`; undefined/null when the browser exposes no track list. */
  audioTrackCount: number | null | undefined;
};

/**
 * Detects a native Direct Play element that is silently playing without a picture or without sound.
 *
 * Browsers cannot be trusted to report either: Chromium decodes only the audio track of a Matroska
 * whose video codec it lacks, and an Edge without the Dolby extension answers `canPlayType("ac-3")`
 * yet decodes no audio frame. Firefox exposes neither counter below, so it is never judged.
 */
export function evaluateNativeSilentFailure(input: NativeSilentFailureInput): NativeSilentFailure | null {
  if (input.hasVideoStream && input.videoWidth === 0 && input.playedMs >= NATIVE_SILENT_VIDEO_MS) {
    return "silent-video";
  }
  if (input.hasAudioStream && input.playedMs >= NATIVE_SILENT_AUDIO_MS) {
    if (typeof input.audioDecodedBytes === "number" && input.audioDecodedBytes === 0) return "silent-audio";
    if (typeof input.audioTrackCount === "number" && input.audioTrackCount === 0) return "silent-audio";
  }
  return null;
}

export function evaluateClientDecodeWatchdog(
  watchdog: ClientDecodeWatchdog,
  nowMs: number,
): ClientDecodeWatchdogVerdict {
  if (
    watchdog.firstSegmentAtMs === null
    && nowMs - watchdog.startedAtMs >= CLIENT_FIRST_SEGMENT_TIMEOUT_MS
  ) {
    return { action: "fallback", reason: "stalled" };
  }
  if (watchdog.fallbackSpeedSamples >= CLIENT_DECODE_SLOW_SAMPLE_COUNT) {
    return { action: "fallback", reason: "slow" };
  }
  if (watchdog.slowSamples >= CLIENT_DECODE_SLOW_SAMPLE_COUNT) return { action: "slow" };
  return { action: "continue" };
}
