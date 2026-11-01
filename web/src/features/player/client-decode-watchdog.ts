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
