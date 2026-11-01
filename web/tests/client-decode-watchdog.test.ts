import { describe, expect, it } from "vitest";
import {
  DIRECT_FIRST_FRAME_TIMEOUT_MS,
  CLIENT_DECODE_SLOW_SAMPLE_COUNT,
  CLIENT_DECODE_SLOW_SPEED_X,
  CLIENT_FIRST_SEGMENT_TIMEOUT_MS,
  evaluateClientDecodeWatchdog,
  evaluateDirectPlayWatchdog,
  markClientDecodeFirstSegment,
  recordClientDecodeSpeedSample,
  startClientDecodeWatchdog,
} from "../src/features/player/client-decode-watchdog";

const started = startClientDecodeWatchdog(1_000);

describe("client decode watchdog", () => {
  it("waits for the first playable segment inside the timeout window", () => {
    expect(evaluateClientDecodeWatchdog(started, 1_000)).toEqual({ action: "continue" });
    expect(evaluateClientDecodeWatchdog(started, 1_000 + CLIENT_FIRST_SEGMENT_TIMEOUT_MS - 1)).toEqual({ action: "continue" });
  });

  it("falls back when no playable segment arrived inside the timeout window", () => {
    expect(evaluateClientDecodeWatchdog(started, 1_000 + CLIENT_FIRST_SEGMENT_TIMEOUT_MS))
      .toEqual({ action: "fallback", reason: "stalled" });
  });

  it("stops watching the first segment once one was produced", () => {
    const ready = markClientDecodeFirstSegment(started, 4_000);

    expect(ready.firstSegmentAtMs).toBe(4_000);
    expect(evaluateClientDecodeWatchdog(ready, 4_000 + CLIENT_FIRST_SEGMENT_TIMEOUT_MS * 10)).toEqual({ action: "continue" });
  });

  it("only warns while decoded speed stays below realtime", () => {
    let state = markClientDecodeFirstSegment(started, 2_000);
    for (let sample = 0; sample < CLIENT_DECODE_SLOW_SAMPLE_COUNT + 3; sample += 1) {
      state = recordClientDecodeSpeedSample(state, 0.7);
    }

    expect(state.slowSamples).toBe(CLIENT_DECODE_SLOW_SAMPLE_COUNT + 3);
    // Slow-but-playable playback keeps the existing "slower than realtime" banner and must not
    // silently switch to server transcoding.
    expect(evaluateClientDecodeWatchdog(state, 30_000)).toEqual({ action: "slow" });
    expect(CLIENT_DECODE_SLOW_SPEED_X).toBeLessThan(1);
  });

  it("falls back after sustained clearly-too-slow samples", () => {
    let state = markClientDecodeFirstSegment(started, 2_000);
    for (let sample = 0; sample < CLIENT_DECODE_SLOW_SAMPLE_COUNT - 1; sample += 1) {
      state = recordClientDecodeSpeedSample(state, 0.3);
    }
    // Two samples are below realtime but not sustained yet: only the banner, no fallback.
    expect(evaluateClientDecodeWatchdog(state, 10_000)).toEqual({ action: "continue" });

    state = recordClientDecodeSpeedSample(state, 0.3);
    expect(evaluateClientDecodeWatchdog(state, 10_000)).toEqual({ action: "fallback", reason: "slow" });
  });

  it("resets the sample counters once playback recovers to realtime", () => {
    let state = markClientDecodeFirstSegment(started, 2_000);
    state = recordClientDecodeSpeedSample(state, 0.3);
    state = recordClientDecodeSpeedSample(state, 0.3);
    state = recordClientDecodeSpeedSample(state, 1.4);

    expect(state.slowSamples).toBe(0);
    expect(state.fallbackSpeedSamples).toBe(0);
    expect(evaluateClientDecodeWatchdog(state, 10_000)).toEqual({ action: "continue" });
  });

  it("only reports a stalled native Direct Play element after its own window", () => {
    const native = startClientDecodeWatchdog(1_000);
    expect(evaluateDirectPlayWatchdog(native, 1_000 + DIRECT_FIRST_FRAME_TIMEOUT_MS - 1)).toBe(false);
    expect(evaluateDirectPlayWatchdog(native, 1_000 + DIRECT_FIRST_FRAME_TIMEOUT_MS)).toBe(true);
    expect(evaluateDirectPlayWatchdog(markClientDecodeFirstSegment(native, 3_000), 60_000)).toBe(false);
  });

  it("ignores non-finite speed samples", () => {
    const state = recordClientDecodeSpeedSample(started, Number.NaN);

    expect(state.slowSamples).toBe(0);
    expect(state.lastSpeedX).toBeNull();
    expect(evaluateClientDecodeWatchdog(state, 1_500)).toEqual({ action: "continue" });
  });
});
