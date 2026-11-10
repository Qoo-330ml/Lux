import { describe, expect, it, vi } from "vitest";
import {
  AUTO_START_PLAYBACK_STATE_KEY,
  attemptAutoStartPlayback,
  autoStartPlaybackState,
  createAutoStartPlaybackGate,
  requestsAutoStartPlayback,
  settleAutoStartPlaybackAfterFailure,
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

  it("peeks at a pending request without consuming it", () => {
    const gate = createAutoStartPlaybackGate(false);
    expect(gate.peek()).toBe(false);

    gate.request();
    gate.request();
    expect(gate.peek()).toBe(true);
    expect(gate.peek()).toBe(true);
    expect(gate.consume()).toBe(true);
    expect(gate.peek()).toBe(false);
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

describe("LuxPlayer one-shot auto start settlement", () => {
  it("keeps the request when the same episode reloads", () => {
    const gate = createAutoStartPlaybackGate(true);

    settleAutoStartPlaybackAfterFailure(gate, true);

    // 回退重载后的会话仍应自动起播一次。
    expect(gate.peek()).toBe(true);
    expect(gate.consume()).toBe(true);
  });

  it("clears the request on a terminal failure", () => {
    for (const gate of [
      createAutoStartPlaybackGate(true),
      (() => {
        const primed = createAutoStartPlaybackGate(false);
        primed.request();
        return primed;
      })(),
    ]) {
      settleAutoStartPlaybackAfterFailure(gate, false);
      // 终态失败后，手动下一集/切音轨/切画质的新会话不得继承这个请求。
      expect(gate.peek()).toBe(false);
      expect(gate.consume()).toBe(false);
    }
  });

  it("is harmless without a pending request", () => {
    const gate = createAutoStartPlaybackGate(false);

    settleAutoStartPlaybackAfterFailure(gate, false);

    expect(gate.peek()).toBe(false);
  });
});
