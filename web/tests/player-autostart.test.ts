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

  it("swallows a browser autoplay rejection and keeps the player paused", async () => {
    const blocked = vi.fn().mockRejectedValue(
      new DOMException("play() failed because the user didn't interact", "NotAllowedError"),
    );

    await expect(attemptAutoStartPlayback(blocked)).resolves.toBe("blocked");
  });

  it("does not rethrow other playback errors either", async () => {
    const failed = vi.fn().mockRejectedValue(new Error("媒体源不可用"));

    await expect(attemptAutoStartPlayback(failed)).resolves.toBe("blocked");
  });
});
