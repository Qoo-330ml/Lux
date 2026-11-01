// @vitest-environment jsdom

/**
 * First-segment watchdog for the client decode engines.
 *
 * `ClientMkvEngine.setSource()` waits on a module Worker and an MSE `SourceBuffer` with no timeout of
 * their own: a Worker that never reports ready, a demuxer that never emits a segment or an append
 * that never fires `updateend` leaves the player on “正在准备客户端解码…” forever. These tests pin the
 * runtime side of the watchdog: after the first-segment window the session is re-requested as server
 * transcoding, with a diagnostic and the degradation notice.
 */
import { act, useRef } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { MediaSource, WebPlaybackPlan } from "../src/lib/api/types";
import type { PlaybackEngine } from "../src/features/player/playback-engine";
import type { LuxPlayerRuntime } from "../src/features/player/core/player-runtime";
import type { PlaybackReporting } from "../src/features/player/usePlaybackReporting";
import { CLIENT_FIRST_SEGMENT_TIMEOUT_MS, DIRECT_FIRST_FRAME_TIMEOUT_MS } from "../src/features/player/client-decode-watchdog";
import { PLAYBACK_PERFORMANCE_EVENT } from "../src/features/player/playback-engine";
import { usePlaybackRuntime } from "../src/features/player/usePlaybackRuntime";

const engineState = vi.hoisted(() => ({
  created: 0,
  /** `hang` reproduces a client engine that never resolves its first segment. */
  mode: "hang" as "hang" | "resolve",
}));

vi.mock("../src/features/player/mkv-playback-engine", () => ({
  ClientMkvEngine: class MockClientMkvEngine {
    readonly kind = "client-mkv";
    readonly error = null;
    readonly performance = null;
    constructor(readonly element: HTMLVideoElement) { engineState.created += 1; }
    setSource() {
      return engineState.mode === "hang" ? new Promise<void>(() => undefined) : Promise.resolve();
    }
    destroy() {}
    play() { return Promise.resolve(); }
    pause() {}
    seek() {}
    snapshot() { return { currentTime: 0, duration: 8, ended: false }; }
  },
}));

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const source: MediaSource = {
  id: "source-watchdog",
  isDefault: true,
  sourceKind: "LOCAL_FILE",
  container: "mkv",
  streams: [
    { index: 0, type: "VIDEO", codec: "h264", details: { Width: 1920, Height: 1080, PixelFormat: "yuv420p" } },
    { index: 1, type: "AUDIO", codec: "AAC" },
  ],
};
const plan: WebPlaybackPlan = { type: "DIRECT", url: "/api/v1/playback/sessions/watchdog/direct" };

function Harness({
  requestServerFallback,
  playbackSource = source,
}: {
  requestServerFallback: (reason?: unknown, clientDecodeFallback?: string) => Promise<boolean>;
  playbackSource?: MediaSource;
}) {
  const playbackDataRef = useRef(undefined) as never;
  const hasRestoredPositionRef = useRef(false);
  const videoRef = useRef<HTMLVideoElement | null>(null);
  const engineRef = useRef<PlaybackEngine | null>(null);
  const runtimeRef = useRef<LuxPlayerRuntime | null>(null);
  const currentTimeRef = useRef(0);
  const durationRef = useRef(0);
  const bufferedEndRef = useRef(0);
  const isDraggingScrubberRef = useRef(false);
  const playbackSessionIdRef = useRef<string | null>(null);
  usePlaybackRuntime({
    playbackKey: "item-watchdog:source-watchdog",
    source: playbackSource,
    playbackPlan: plan,
    streamUrl: plan.type === "DIRECT" ? plan.url : "",
    poster: null,
    candidate: undefined,
    candidates: [],
    hasNextCandidate: false,
    remoteHttpSource: false,
    clientMkvSourceUrl: null,
    playbackDataRef,
    hasRestoredPositionRef,
    videoRef,
    engineRef,
    runtimeRef,
    currentTimeRef,
    durationRef,
    bufferedEndRef,
    isDraggingScrubberRef,
    playbackSessionIdRef,
    reportPlayback: (async () => undefined) as unknown as PlaybackReporting["reportPlayback"],
    stopActiveSession: (async () => undefined) as unknown as PlaybackReporting["stopActiveSession"],
    markStarted: () => undefined,
    requestServerFallback: requestServerFallback as never,
    setCurrentTime: () => undefined,
    setDuration: () => undefined,
    setBufferedEnd: () => undefined,
    setPlaying: () => undefined,
    setControlsVisible: () => undefined,
    setControlActivity: () => undefined,
    setFailedStreamUrl: () => undefined,
    setPlaybackFailure: () => undefined,
    setFallbackLoading: () => undefined,
    setFallbackSpeedX: () => undefined,
    setClientEngineActive: () => undefined,
  });
  return <video ref={videoRef} />;
}

describe("client decode first-segment watchdog", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;

  beforeEach(() => {
    engineState.created = 0;
    engineState.mode = "hang";
    vi.stubGlobal("MediaSource", { isTypeSupported: (mime: string) => mime.includes("avc1") });
    vi.stubGlobal("Worker", class Worker {});
    vi.stubGlobal("VideoEncoder", class VideoEncoder {
      static isConfigSupported() { return Promise.resolve({ supported: true }); }
    });
    vi.spyOn(HTMLMediaElement.prototype, "load").mockImplementation(() => undefined);
    vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(() => undefined);
    vi.spyOn(HTMLMediaElement.prototype, "play").mockResolvedValue(undefined);
  });

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
    vi.useRealTimers();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  async function renderHarness(onFallback: () => void, playbackSource?: MediaSource) {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const requestServerFallback = vi.fn(async () => {
      onFallback();
      return true;
    });
    await act(async () => {
      root?.render(<Harness requestServerFallback={requestServerFallback} playbackSource={playbackSource} />);
    });
    return requestServerFallback;
  }

  it("falls back to server transcoding when no playable segment arrives in time", async () => {
    vi.useFakeTimers();
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const onFallback = vi.fn();
    const requestServerFallback = await renderHarness(onFallback);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(100);
    });
    expect(engineState.created).toBe(1);
    expect(requestServerFallback).not.toHaveBeenCalled();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(CLIENT_FIRST_SEGMENT_TIMEOUT_MS + 100);
    });

    expect(requestServerFallback).toHaveBeenCalledTimes(1);
    expect(requestServerFallback.mock.calls[0]?.[1]).toBe("stalled");
    expect(warn).toHaveBeenCalledWith(expect.stringContaining("stalled"));
    expect(onFallback).toHaveBeenCalledTimes(1);
  });

  it("falls back when decoded throughput stays clearly below realtime", async () => {
    vi.useFakeTimers();
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const requestServerFallback = await renderHarness(() => undefined);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(100);
    });
    const video = container?.querySelector("video");
    expect(video).not.toBeNull();

    for (let sample = 0; sample < 3; sample += 1) {
      await act(async () => {
        video?.dispatchEvent(new CustomEvent(PLAYBACK_PERFORMANCE_EVENT, {
          detail: { mediaDurationMs: 3_000, processingDurationMs: 10_000, speedX: 0.3, realtime: false },
        }));
        await vi.advanceTimersByTimeAsync(10);
      });
    }

    expect(requestServerFallback).toHaveBeenCalledTimes(1);
    expect(requestServerFallback.mock.calls[0]?.[1]).toBe("slow");
    expect(warn).toHaveBeenCalledWith(expect.stringContaining("speedX 0.30"));
  });

  it("keeps playing a client decode that is slower than realtime but still usable", async () => {
    vi.useFakeTimers();
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const requestServerFallback = await renderHarness(() => undefined);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(100);
    });
    const video = container?.querySelector("video");

    for (let sample = 0; sample < 6; sample += 1) {
      await act(async () => {
        video?.dispatchEvent(new CustomEvent(PLAYBACK_PERFORMANCE_EVENT, {
          detail: { mediaDurationMs: 7_000, processingDurationMs: 10_000, speedX: 0.7, realtime: false },
        }));
        await vi.advanceTimersByTimeAsync(10);
      });
    }

    // The existing “客户端解码速度低于实时” banner already covers this case.
    expect(requestServerFallback).not.toHaveBeenCalled();
    expect(warn).not.toHaveBeenCalled();
  });

  it("falls back when a native Direct Play element never delivers a frame", async () => {
    vi.useFakeTimers();
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    // A Chromium build that opens the Matroska container but cannot decode its video track: no
    // `error`, no `loadedmetadata`, and the element stays at readyState 0.
    vi.spyOn(HTMLMediaElement.prototype, "canPlayType").mockReturnValue("probably");
    const matroska: MediaSource = {
      id: "source-vp9",
      isDefault: true,
      sourceKind: "LOCAL_FILE",
      container: "mkv",
      streams: [
        { index: 0, type: "VIDEO", codec: "vp9", details: { Width: 1280, Height: 720 } },
        { index: 1, type: "AUDIO", codec: "opus" },
      ],
    };
    const requestServerFallback = await renderHarness(() => undefined, matroska);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(DIRECT_FIRST_FRAME_TIMEOUT_MS - 1_000);
    });
    expect(requestServerFallback).not.toHaveBeenCalled();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(2_000);
    });
    expect(requestServerFallback).toHaveBeenCalledTimes(1);
    expect(requestServerFallback.mock.calls[0]?.[1]).toBe("stalled");
    expect(warn).toHaveBeenCalledWith(expect.stringContaining("direct play produced no frame"));
  });

  it("keeps a client engine that produces its first segment in time", async () => {
    vi.useFakeTimers();
    engineState.mode = "resolve";
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const requestServerFallback = await renderHarness(() => undefined);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(CLIENT_FIRST_SEGMENT_TIMEOUT_MS * 3);
    });

    expect(engineState.created).toBe(1);
    expect(requestServerFallback).not.toHaveBeenCalled();
    expect(warn).not.toHaveBeenCalled();
  });
});
