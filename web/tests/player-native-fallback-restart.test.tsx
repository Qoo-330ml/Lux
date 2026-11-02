// @vitest-environment jsdom

/**
 * A native Direct Play element that plays *silently* (no video frame, or no audio frame at all) or
 * that never reaches `HAVE_CURRENT_DATA` must be reloaded as the server transcoding plan the
 * fallback session returns.
 *
 * The real-site report: a 10 GB Matroska (H.264 + DTS-HD MA) was opened natively, the silent-audio
 * watchdog asked for a fallback, the server answered `SERVER_HLS` (video copy + audio transcode) —
 * and the page then requested nothing at all: no `hls-playback-engine` chunk, no `index.m3u8`, and
 * the element stayed at `readyState 0` behind the “服务端正在转码音轨” banner. The engine switch must
 * therefore run for *every* fallback trigger, not only for an engine `error` during `load()`.
 */
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { PlayerPage } from "../src/features/player/PlayerPage";
import { api } from "../src/lib/api/client";
import { mockPlaybackBootstrap } from "./player-test-helpers";
import {
  DIRECT_FIRST_FRAME_TIMEOUT_MS,
  NATIVE_SILENT_AUDIO_MS,
  NATIVE_SILENT_POLL_MS,
  NATIVE_SILENT_VIDEO_MS,
} from "../src/features/player/client-decode-watchdog";

const engineState = vi.hoisted(() => ({
  mkvEngines: 0,
  hevcEngines: 0,
  hlsEngines: 0,
  /** Every URL handed to the HLS engine, in order. */
  hlsSources: [] as string[],
  hlsPlays: 0,
}));

vi.mock("../src/features/player/mkv-playback-engine", () => ({
  ClientMkvEngine: class MockClientMkvEngine {
    readonly kind = "client-mkv";
    readonly error = null;
    readonly performance = null;
    constructor(readonly element: HTMLVideoElement) { engineState.mkvEngines += 1; }
    setSource() { return Promise.resolve(); }
    destroy() {}
    play() { return Promise.resolve(); }
    pause() {}
    seek() {}
    snapshot() { return { currentTime: 0, duration: 8, ended: false }; }
  },
}));

vi.mock("../src/features/player/hevc-playback-engine", () => ({
  ClientHevcEngine: class MockClientHevcEngine {
    readonly kind = "client-hevc";
    readonly error = null;
    readonly performance = null;
    constructor(readonly element: HTMLVideoElement) { engineState.hevcEngines += 1; }
    setSource() { return Promise.resolve(); }
    destroy() {}
    play() { return Promise.resolve(); }
    pause() {}
    seek() {}
    snapshot() { return { currentTime: 0, duration: 8, ended: false }; }
  },
}));

vi.mock("../src/features/player/hls-playback-engine", () => ({
  HlsVideoEngine: class MockHlsVideoEngine {
    readonly kind = "native";
    readonly performance = null;
    readonly error = null;
    constructor(readonly element: HTMLVideoElement) { engineState.hlsEngines += 1; }
    setSource(url: string) {
      engineState.hlsSources.push(url);
      return Promise.resolve();
    }
    destroy() {}
    play() {
      engineState.hlsPlays += 1;
      return Promise.resolve();
    }
    pause() {}
    seek() {}
    snapshot() { return { currentTime: 0, duration: 8, ended: false }; }
  },
}));

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const DIRECT_URL = "/api/v1/playback/sessions/web-direct/direct?expires=1900000000&signature=test";
const MANIFEST_URL = "/api/v1/playback/sessions/web-server/hls/index.m3u8?expires=1900000000&signature=test";
const DIRECT_PLAN = { type: "DIRECT" as const, url: DIRECT_URL };
const SERVER_PLAN = { type: "SERVER_HLS" as const, manifestUrl: MANIFEST_URL, tier: 2 };

/** H.264 1920x754 + DTS-HD MA, the real Matroska from the report. */
function dtsMatroska() {
  return {
    id: "source-dts",
    isDefault: true,
    sourceKind: "LOCAL_FILE",
    container: "matroska,webm",
    durationTicks: 900_000_000,
    streams: [
      { index: 0, type: "VIDEO", codec: "h264", details: { Width: 1920, Height: 754, PixelFormat: "yuv420p" } },
      { index: 1, type: "AUDIO", codec: "dts", isDefault: true },
      { index: 2, type: "SUBTITLE", codec: "hdmv_pgs_subtitle" },
    ],
  };
}

describe("PlayerPage native Direct Play fallback reload", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;
  let warn: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    window.localStorage.clear();
    engineState.mkvEngines = 0;
    engineState.hevcEngines = 0;
    engineState.hlsEngines = 0;
    engineState.hlsSources = [];
    engineState.hlsPlays = 0;
    mockPlaybackBootstrap();
    warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    vi.stubGlobal("MediaSource", { isTypeSupported: (mime: string) => mime.includes("avc1") });
    vi.stubGlobal("Worker", class Worker {});
    vi.stubGlobal("VideoEncoder", class VideoEncoder {
      static isConfigSupported() { return Promise.resolve({ supported: true }); }
    });
    // Chromium claims the Matroska container and the DTS audio track, then decodes no audio frame.
    vi.spyOn(HTMLMediaElement.prototype, "canPlayType").mockImplementation((mime: string) =>
      mime.startsWith("video/x-matroska") ? "probably" : "",
    );
    vi.spyOn(HTMLMediaElement.prototype, "load").mockImplementation(() => undefined);
    vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(() => undefined);
    vi.spyOn(HTMLMediaElement.prototype, "play").mockResolvedValue(undefined);
    vi.spyOn(api, "playback").mockResolvedValue({ positionTicks: 0, isPlayed: false, state: null, isPaused: false });
    vi.spyOn(api, "webPlaybackEvent").mockResolvedValue({ accepted: true, duplicate: false, stale: false });
    vi.spyOn(api, "stopWebPlaybackSession").mockResolvedValue(undefined);
    vi.spyOn(api, "progress").mockResolvedValue(undefined);
  });

  afterEach(() => {
    vi.useRealTimers();
    if (root) act(() => root?.unmount());
    container?.remove();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  /** First session is native Direct Play, the retry is the server plan from the real report. */
  function directThenServerHls() {
    let calls = 0;
    return vi.spyOn(api, "createWebPlaybackSession").mockImplementation(async (_itemId, sourceId) => {
      calls += 1;
      if (calls > 1) {
        // The real fallback session is a network round trip: it resolves in a later task, so the
        // page renders the pending session (and therefore unmounts the video) in between.
        await new Promise((resolve) => setTimeout(resolve, 600));
      }
      return {
        sessionId: calls === 1 ? "web-direct" : "web-server",
        playSessionId: `lux-web:${calls === 1 ? "web-direct" : "web-server"}`,
        sourceId,
        tier: calls === 1 ? 0 : 2,
        expiresAt: 1_900_000_000,
        plan: calls === 1 ? DIRECT_PLAN : SERVER_PLAN,
      };
    });
  }

  /**
   * The player only enables the item query on the fallback attempt (`playbackAttempt !== 0`); on a
   * real site it is fetched then. The bootstrap read resolves right away, the fallback read returns
   * after the replacement session, exactly as the network did in the report.
   */
  function stubItem(item: Record<string, unknown>) {
    let calls = 0;
    return vi.spyOn(api, "item").mockImplementation(async () => {
      calls += 1;
      if (calls > 1) {
        await new Promise((resolve) => setTimeout(resolve, 800));
      }
      return item as never;
    });
  }

  async function renderPlayer(itemId: string) {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={[`/watch/${itemId}`]}>
            <Routes>
              <Route path="watch/:itemId" element={<PlayerPage />} />
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await flush(2);
    const video = container.querySelector("video");
    expect(video).not.toBeNull();
    return video as HTMLVideoElement;
  }

  async function flush(rounds = 20) {
    for (let round = 0; round < rounds; round += 1) {
      await act(async () => {
        await vi.advanceTimersByTimeAsync(50);
      });
    }
  }

  /**
   * Fakes the DOM media properties jsdom does not implement for an element that is *playing*: the
   * silent-failure watchdog only counts media time that advanced while `paused === false`.
   */
  function fakePlayingElement(
    video: HTMLVideoElement,
    counters: { videoWidth: number; audioDecodedBytes?: number; audioTrackCount?: number; readyState?: number },
  ) {
    const media = { currentTime: 0 };
    Object.defineProperty(video, "paused", { configurable: true, get: () => false });
    Object.defineProperty(video, "seeking", { configurable: true, get: () => false });
    Object.defineProperty(video, "currentTime", { configurable: true, get: () => media.currentTime });
    Object.defineProperty(video, "videoWidth", { configurable: true, get: () => counters.videoWidth });
    Object.defineProperty(video, "readyState", { configurable: true, get: () => counters.readyState ?? 4 });
    if (counters.audioDecodedBytes !== undefined) {
      Object.defineProperty(video, "webkitAudioDecodedByteCount", {
        configurable: true,
        get: () => counters.audioDecodedBytes,
      });
    }
    if (counters.audioTrackCount !== undefined) {
      Object.defineProperty(video, "audioTracks", {
        configurable: true,
        get: () => ({ length: counters.audioTrackCount }),
      });
    }
    return media;
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

  function expectHlsRestarted(session: ReturnType<typeof directThenServerHls>) {
    expect(session).toHaveBeenCalledTimes(2);
    expect(engineState.hlsEngines).toBe(1);
    expect(engineState.hlsSources).toEqual([MANIFEST_URL]);
    // The viewer was already watching, so the degraded session starts on its own.
    expect(engineState.hlsPlays).toBeGreaterThanOrEqual(1);
  }

  it("restarts the new SERVER_HLS plan after a silent-audio fallback", async () => {
    vi.useFakeTimers();
    const session = directThenServerHls();
    stubItem({
      id: "movie-dts",
      title: "DTS-HD MA 电影",
      itemType: "MOVIE",
      mediaSources: [dtsMatroska()],
    });

    const video = await renderPlayer("movie-dts");
    const directElement = video;
    expect(directElement.getAttribute("src")).toBe(DIRECT_URL);
    const media = fakePlayingElement(video, { videoWidth: 1920, audioDecodedBytes: 0 });
    await act(async () => {
      video.dispatchEvent(new Event("loadedmetadata"));
    });

    await playFor(media, NATIVE_SILENT_AUDIO_MS + NATIVE_SILENT_POLL_MS * 2);
    await flush();

    expect(session.mock.calls[1]?.[2]).toMatchObject({ audioCopyToFmp4: false });
    expectHlsRestarted(session);
    expect(warn).toHaveBeenCalledWith(expect.stringContaining("silent-audio"));
    // The degraded plan is what the viewer is told, and it is what is playing.
    expect(container?.textContent).toContain("已切换为服务端音频转码");
    // The native engine is torn down completely, so nothing keeps reading the /direct stream.
    expect(directElement.getAttribute("src")).toBeNull();
  });

  it("restarts the new SERVER_HLS plan after a silent-video fallback", async () => {
    vi.useFakeTimers();
    const session = directThenServerHls();
    stubItem({
      id: "movie-silent-video",
      title: "只有声音没有画面",
      itemType: "MOVIE",
      mediaSources: [dtsMatroska()],
    });

    const video = await renderPlayer("movie-silent-video");
    const media = fakePlayingElement(video, { videoWidth: 0, audioDecodedBytes: 48_000 });
    await act(async () => {
      video.dispatchEvent(new Event("loadedmetadata"));
    });

    await playFor(media, NATIVE_SILENT_VIDEO_MS + NATIVE_SILENT_POLL_MS * 2);
    await flush();

    expectHlsRestarted(session);
    expect(warn).toHaveBeenCalledWith(expect.stringContaining("silent-video"));
  });

  it("restarts the new SERVER_HLS plan after the direct first-frame watchdog fires", async () => {
    vi.useFakeTimers();
    const session = directThenServerHls();
    stubItem({
      id: "movie-no-frame",
      title: "直连没有画面",
      itemType: "MOVIE",
      mediaSources: [dtsMatroska()],
    });

    const video = await renderPlayer("movie-no-frame");
    // Stuck at readyState 0: no `loadedmetadata`, no `error`, no decoded frame.
    fakePlayingElement(video, { videoWidth: 0, readyState: 0 });

    await act(async () => {
      await vi.advanceTimersByTimeAsync(DIRECT_FIRST_FRAME_TIMEOUT_MS - 1_000);
    });
    expect(session).toHaveBeenCalledTimes(1);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(2_000);
    });
    await flush();

    expectHlsRestarted(session);
    expect(warn).toHaveBeenCalledWith(expect.stringContaining("direct play produced no frame"));
  });
});
