import { describe, expect, it } from "vitest";
import {
  DIRECT_FIRST_FRAME_TIMEOUT_MS,
  CLIENT_DECODE_SLOW_SAMPLE_COUNT,
  CLIENT_DECODE_SLOW_SPEED_X,
  CLIENT_FIRST_SEGMENT_TIMEOUT_MS,
  NATIVE_SILENT_AUDIO_MS,
  NATIVE_SILENT_VIDEO_MS,
  evaluateClientDecodeWatchdog,
  evaluateDirectPlayWatchdog,
  evaluateNativeSilentFailure,
  markClientDecodeFirstSegment,
  recordClientDecodeSpeedSample,
  sampleNativeSilentPlayback,
  startClientDecodeWatchdog,
  startNativeSilentPlaybackClock,
  type NativeSilentFailureInput,
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

/** A native Direct Play sample with a healthy picture unless the case overrides it. */
const nativePlayback = (overrides: Partial<NativeSilentFailureInput> = {}): NativeSilentFailureInput => ({
  hasVideoStream: true,
  hasAudioStream: true,
  playedMs: NATIVE_SILENT_AUDIO_MS,
  videoWidth: 1920,
  audioDecodedBytes: 48_000,
  audioTrackCount: undefined,
  ...overrides,
});

describe("native silent playback watchdog", () => {
  it("waits for the two-second video window before blaming a missing frame", () => {
    expect(evaluateNativeSilentFailure(nativePlayback({ playedMs: NATIVE_SILENT_VIDEO_MS - 1, videoWidth: 0 })))
      .toBeNull();
  });

  it("reports a missing video frame once playback passed the window", () => {
    expect(evaluateNativeSilentFailure(nativePlayback({ playedMs: NATIVE_SILENT_VIDEO_MS, videoWidth: 0 })))
      .toBe("silent-video");
  });

  it("keeps a native element that decodes video frames", () => {
    expect(evaluateNativeSilentFailure(nativePlayback({ videoWidth: 1280 }))).toBeNull();
  });

  it("never blames the video track of a source without one", () => {
    expect(evaluateNativeSilentFailure(nativePlayback({ hasVideoStream: false, videoWidth: 0 }))).toBeNull();
  });

  it("waits for the three-second audio window before blaming silent audio", () => {
    expect(evaluateNativeSilentFailure(nativePlayback({ playedMs: NATIVE_SILENT_AUDIO_MS - 1, audioDecodedBytes: 0 })))
      .toBeNull();
  });

  it("reports silent audio when Chromium decoded no audio bytes", () => {
    expect(evaluateNativeSilentFailure(nativePlayback({ audioDecodedBytes: 0 }))).toBe("silent-audio");
  });

  it("reports silent audio when Safari lists no audio track", () => {
    expect(evaluateNativeSilentFailure(nativePlayback({ audioDecodedBytes: undefined, audioTrackCount: 0 })))
      .toBe("silent-audio");
  });

  it("keeps audio that produced decoded bytes or a track", () => {
    expect(evaluateNativeSilentFailure(nativePlayback({ audioDecodedBytes: 16_384 }))).toBeNull();
    expect(evaluateNativeSilentFailure(nativePlayback({ audioDecodedBytes: undefined, audioTrackCount: 2 })))
      .toBeNull();
  });

  it("does not judge a browser that exposes neither audio counter", () => {
    // Firefox has neither `webkitAudioDecodedByteCount` nor `audioTracks`: silence is unmeasurable.
    expect(evaluateNativeSilentFailure(nativePlayback({ audioDecodedBytes: undefined, audioTrackCount: undefined })))
      .toBeNull();
  });

  it("never blames silent audio for a source without an audio track", () => {
    expect(evaluateNativeSilentFailure(nativePlayback({ hasAudioStream: false, audioDecodedBytes: 0 })))
      .toBeNull();
  });

  it("counts only media time that really advanced", () => {
    let clock = startNativeSilentPlaybackClock();
    clock = sampleNativeSilentPlayback(clock, { nowMs: 1_000, currentTime: 0, paused: false, seeking: false });
    clock = sampleNativeSilentPlayback(clock, { nowMs: 1_500, currentTime: 0.5, paused: false, seeking: false });
    expect(clock.playedMs).toBe(500);

    // A stalled element, a paused one and a seek do not add playback time.
    clock = sampleNativeSilentPlayback(clock, { nowMs: 2_000, currentTime: 0.5, paused: false, seeking: false });
    clock = sampleNativeSilentPlayback(clock, { nowMs: 2_500, currentTime: 1.0, paused: true, seeking: false });
    clock = sampleNativeSilentPlayback(clock, { nowMs: 3_000, currentTime: 60, paused: false, seeking: true });
    expect(clock.playedMs).toBe(500);

    clock = sampleNativeSilentPlayback(clock, { nowMs: 3_500, currentTime: 60.5, paused: false, seeking: false });
    expect(clock.playedMs).toBe(1_000);
  });

  it("caps one sample so a suspended tab does not count as playback", () => {
    let clock = startNativeSilentPlaybackClock();
    clock = sampleNativeSilentPlayback(clock, { nowMs: 1_000, currentTime: 0, paused: false, seeking: false });
    clock = sampleNativeSilentPlayback(clock, { nowMs: 600_000, currentTime: 1, paused: false, seeking: false });

    expect(clock.playedMs).toBeLessThanOrEqual(2_000);
  });
});
