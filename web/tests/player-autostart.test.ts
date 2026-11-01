import { describe, expect, it, vi } from "vitest";
import {
  AUTO_START_PLAYBACK_STATE_KEY,
  attemptAutoStartPlayback,
  autoStartPlaybackState,
  createAutoStartPlaybackGate,
  requestsAutoStartPlayback,
} from "../src/features/player/player-autostart";

describe("LuxPlayer auto start playback marker", () => {
  it("marks the target route with a one-shot auto start flag", () => {
    const state = autoStartPlaybackState();

    expect(AUTO_START_PLAYBACK_STATE_KEY).toBe("luxAutoStartPlayback");
    expect(state).toEqual({ luxAutoStartPlayback: true });
    expect(requestsAutoStartPlayback(state)).toBe(true);
  });

  it("ignores missing, unrelated, or falsy route state", () => {
    for (const state of [
      undefined,
      null,
      {},
      { other: true },
      { luxAutoStartPlayback: false },
      { luxAutoStartPlayback: "true" },
      "luxAutoStartPlayback",
      42,
    ]) {
      expect(requestsAutoStartPlayback(state)).toBe(false);
    }
  });

  it("consumes the request only once so a later source switch does not replay it", () => {
    const gate = createAutoStartPlaybackGate(false);
    expect(gate.consume()).toBe(false);

    gate.request();
    expect(gate.consume()).toBe(true);
    expect(gate.consume()).toBe(false);

    const primed = createAutoStartPlaybackGate(true);
    expect(primed.consume()).toBe(true);
    expect(primed.consume()).toBe(false);
  });
});

describe("LuxPlayer auto start attempt", () => {
  it("reports a started playback", async () => {
    const play = vi.fn().mockResolvedValue(undefined);

    await expect(attemptAutoStartPlayback(play)).resolves.toBe("played");
    expect(play).toHaveBeenCalledTimes(1);
  });

  it("treats a browser autoplay policy rejection as a silent fallback", async () => {
    const notAllowed = vi.fn().mockRejectedValue(
      new DOMException("play() failed because the user didn't interact", "NotAllowedError"),
    );
    const aborted = vi.fn().mockRejectedValue(
      new DOMException("The play() request was interrupted", "AbortError"),
    );

    await expect(attemptAutoStartPlayback(notAllowed)).resolves.toBe("blocked");
    await expect(attemptAutoStartPlayback(aborted)).resolves.toBe("blocked");
  });

  it("lets other playback errors reach the existing failure path", async () => {
    const unsupported = new DOMException("no supported source", "NotSupportedError");

    await expect(attemptAutoStartPlayback(vi.fn().mockRejectedValue(unsupported)))
      .rejects.toBe(unsupported);
    await expect(attemptAutoStartPlayback(vi.fn().mockRejectedValue(new Error("媒体源不可用"))))
      .rejects.toThrow("媒体源不可用");
  });
});
