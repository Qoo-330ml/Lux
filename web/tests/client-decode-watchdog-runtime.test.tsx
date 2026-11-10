// @vitest-environment jsdom

/**
 * Watchdogs for the client decode engines and for the native Direct Play element.
 *
 * `ClientMkvEngine.setSource()` waits on a module Worker and an MSE `SourceBuffer` with no timeout of
 * their own: a Worker that never reports ready, a demuxer that never emits a segment or an append
 * that never fires `updateend` leaves the player on “正在准备客户端解码…” forever. A native Direct
 * Play element fails the other way: Chromium plays only the audio track of a Matroska it cannot
 * decode, and Edge claims AC-3 while decoding no audio frame — neither fires `error`. These tests pin
 * the runtime side of both watchdogs: the session is re-requested as server transcoding, with a
 * diagnostic and the degradation notice.
 */
import { act, useCallback, useRef, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { MediaSource, WebPlaybackPlan } from "../src/lib/api/types";
import { NativeVideoEngine, type PlaybackEngine } from "../src/features/player/playback-engine";
import type { LuxPlayerRuntime } from "../src/features/player/core/player-runtime";
import type { PlaybackReporting } from "../src/features/player/usePlaybackReporting";
import {
  CLIENT_FIRST_SEGMENT_TIMEOUT_MS,
  DIRECT_FIRST_FRAME_TIMEOUT_MS,
  NATIVE_SILENT_AUDIO_MS,
  NATIVE_SILENT_POLL_MS,
  NATIVE_SILENT_VIDEO_MS,
} from "../src/features/player/client-decode-watchdog";
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
  remoteHttpSource = false,
}: {
  requestServerFallback: (reason?: unknown, clientDecodeFallback?: string, options?: unknown) => Promise<boolean>;
  playbackSource?: MediaSource;
  remoteHttpSource?: boolean;
}) {
  const playbackDataRef = useRef(undefined) as never;
  const hasRestoredPositionRef = useRef(false);
  // The runtime only loads a plan once the element is published, exactly like
  // `usePlayerVideoElement` in the player page.
  const [videoElement, setVideoElement] = useState<HTMLVideoElement | null>(null);
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
    remoteHttpSource,
    clientMkvSourceUrl: null,
    playbackDataRef,
    hasRestoredPositionRef,
    videoRef,
    videoElement,
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
  const bindVideo = useCallback((video: HTMLVideoElement | null) => {
    videoRef.current = video;
    if (video) engineRef.current = new NativeVideoEngine(video);
    setVideoElement(video);
  }, [engineRef]);
  return <video ref={bindVideo} />;
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

  async function renderHarness(
    onFallback: () => void,
    playbackSource?: MediaSource,
    remoteHttpSource = false,
  ) {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const requestServerFallback = vi.fn(async () => {
      onFallback();
      return true;
    });
    await act(async () => {
      root?.render(
        <Harness
          requestServerFallback={requestServerFallback}
          playbackSource={playbackSource}
          remoteHttpSource={remoteHttpSource}
        />,
      );
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

  it("keeps waiting on a remote STRM source that has no server fallback", async () => {
    vi.useFakeTimers();
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const remote: MediaSource = {
      id: "source-remote",
      isDefault: true,
      sourceKind: "STRM_URL",
      externalUrl: "https://media.example.test/video.mp4",
      container: "mp4",
      streams: [{ index: 0, type: "VIDEO", codec: "h264", details: { Width: 1920, Height: 1080 } }],
    };
    const requestServerFallback = await renderHarness(() => undefined, remote, true);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(DIRECT_FIRST_FRAME_TIMEOUT_MS * 3);
    });

    expect(requestServerFallback).not.toHaveBeenCalled();
    expect(warn).not.toHaveBeenCalled();
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

describe("native direct play silent failure watchdog", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;

  const matroska = (videoCodec: string, audioCodec: string): MediaSource => ({
    id: "source-silent",
    isDefault: true,
    sourceKind: "LOCAL_FILE",
    container: "mkv",
    streams: [
      { index: 0, type: "VIDEO", codec: videoCodec, details: { Width: 1920, Height: 1080 } },
      { index: 1, type: "AUDIO", codec: audioCodec },
    ],
  });

  beforeEach(() => {
    engineState.created = 0;
    engineState.mode = "resolve";
    // Chromium-like: MSE decodes H.264 and (falsely, as Edge without the Dolby extension does) AC-3.
    vi.stubGlobal("MediaSource", {
      isTypeSupported: (mime: string) => mime.includes("avc1") || mime.includes("ac-3"),
    });
    vi.stubGlobal("Worker", class Worker {});
    vi.stubGlobal("VideoEncoder", class VideoEncoder {
      static isConfigSupported() { return Promise.resolve({ supported: true }); }
    });
    vi.spyOn(HTMLMediaElement.prototype, "load").mockImplementation(() => undefined);
    vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(() => undefined);
    vi.spyOn(HTMLMediaElement.prototype, "play").mockResolvedValue(undefined);
    // The element claims the whole container, so the native path is taken.
    vi.spyOn(HTMLMediaElement.prototype, "canPlayType").mockReturnValue("probably");
  });

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
    vi.useRealTimers();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  /**
   * Renders the runtime over a native element that plays, and fakes the DOM properties jsdom does not
   * implement (`paused`, media time, decoded-audio counters).
   */
  async function renderNativePlayback(
    playbackSource: MediaSource,
    element: {
      videoWidth: number;
      audioDecodedBytes?: number;
      audioTrackCount?: number;
      readyState?: number;
    },
  ) {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const requestServerFallback = vi.fn(async () => true);
    await act(async () => {
      root?.render(
        <Harness requestServerFallback={requestServerFallback} playbackSource={playbackSource} />,
      );
    });
    const video = container.querySelector("video") as HTMLVideoElement | null;
    expect(video).not.toBeNull();
    const media = { currentTime: 0 };
    Object.defineProperty(video!, "paused", { configurable: true, get: () => false });
    Object.defineProperty(video!, "seeking", { configurable: true, get: () => false });
    Object.defineProperty(video!, "currentTime", { configurable: true, get: () => media.currentTime });
    Object.defineProperty(video!, "videoWidth", { configurable: true, get: () => element.videoWidth });
    Object.defineProperty(video!, "readyState", { configurable: true, get: () => element.readyState ?? 4 });
    if (element.audioDecodedBytes !== undefined) {
      Object.defineProperty(video!, "webkitAudioDecodedByteCount", {
        configurable: true,
        get: () => element.audioDecodedBytes,
      });
    }
    if (element.audioTrackCount !== undefined) {
      Object.defineProperty(video!, "audioTracks", {
        configurable: true,
        get: () => ({ length: element.audioTrackCount }),
      });
    }
    await act(async () => {
      video!.dispatchEvent(new Event("loadedmetadata"));
    });
    return { requestServerFallback, video: video!, media };
  }

  /** Advances wall clock and media time together, one watchdog poll at a time. */
  async function playFor(media: { currentTime: number }, ms: number) {
    for (let elapsed = 0; elapsed < ms; elapsed += NATIVE_SILENT_POLL_MS) {
      media.currentTime += NATIVE_SILENT_POLL_MS / 1_000;
      await act(async () => {
        await vi.advanceTimersByTimeAsync(NATIVE_SILENT_POLL_MS);
      });
    }
  }

  it("falls back to server transcoding when a playing element has no video frame", async () => {
    vi.useFakeTimers();
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const { requestServerFallback, media } = await renderNativePlayback(
      matroska("h264", "aac"),
      { videoWidth: 0, audioDecodedBytes: 48_000 },
    );

    await playFor(media, NATIVE_SILENT_VIDEO_MS - NATIVE_SILENT_POLL_MS);
    expect(requestServerFallback).not.toHaveBeenCalled();

    await playFor(media, NATIVE_SILENT_POLL_MS * 2);

    expect(requestServerFallback).toHaveBeenCalledTimes(1);
    expect(requestServerFallback.mock.calls[0]?.[1]).toBe("silent-video");
    expect(warn).toHaveBeenCalledWith(expect.stringContaining("silent-video"));
    expect(warn).toHaveBeenCalledWith(expect.stringContaining("no video frame"));
  });

  it("falls back and forces audio transcoding when the element decodes no audio", async () => {
    vi.useFakeTimers();
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const { requestServerFallback, media } = await renderNativePlayback(
      // Win11 Edge: the element plays the picture but never decodes the AC-3 track.
      matroska("h264", "ac3"),
      { videoWidth: 1920, audioDecodedBytes: 0 },
    );

    await playFor(media, NATIVE_SILENT_AUDIO_MS - NATIVE_SILENT_POLL_MS);
    expect(requestServerFallback).not.toHaveBeenCalled();

    await playFor(media, NATIVE_SILENT_POLL_MS * 2);

    expect(requestServerFallback).toHaveBeenCalledTimes(1);
    expect(requestServerFallback.mock.calls[0]?.[1]).toBe("silent-audio");
    expect(requestServerFallback.mock.calls[0]?.[2]).toEqual({ forceAudioTranscode: true });
    expect(warn).toHaveBeenCalledWith(expect.stringContaining("silent-audio"));
  });

  it("treats an empty Safari audio track list as silent audio", async () => {
    vi.useFakeTimers();
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const { requestServerFallback, media } = await renderNativePlayback(
      matroska("h264", "aac"),
      // Safari: a playback-capable element whose audio track list is empty.
      { videoWidth: 1920, audioTrackCount: 0, readyState: 4 },
    );

    await playFor(media, NATIVE_SILENT_AUDIO_MS + NATIVE_SILENT_POLL_MS);

    expect(requestServerFallback).toHaveBeenCalledTimes(1);
    expect(requestServerFallback.mock.calls[0]?.[1]).toBe("silent-audio");
    expect(warn).toHaveBeenCalledWith(expect.stringContaining("silent-audio"));
  });

  it("keeps a normal H.264 plus AAC native playback untouched", async () => {
    vi.useFakeTimers();
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const { requestServerFallback, media } = await renderNativePlayback(
      matroska("h264", "aac"),
      { videoWidth: 1920, audioDecodedBytes: 96_000 },
    );

    await playFor(media, NATIVE_SILENT_AUDIO_MS * 4);

    expect(requestServerFallback).not.toHaveBeenCalled();
    expect(warn).not.toHaveBeenCalled();
  });
});
