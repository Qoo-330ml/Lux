// @vitest-environment jsdom

/**
 * End-to-end gate behaviour for the client decode path.
 *
 * These tests run the *real* `playback-selection` decision against a simulated browser, so a source
 * the browser cannot decode (10-bit H.264, 4K HEVC without HEVC MSE) must re-request the session as
 * server-side transcoding instead of entering a client engine that never produces a picture.
 */
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { PlayerPage } from "../src/features/player/PlayerPage";
import { api } from "../src/lib/api/client";
import { mockPlaybackBootstrap } from "./player-test-helpers";

const engineState = vi.hoisted(() => ({
  mkvEngines: 0,
  hevcEngines: 0,
  hlsEngines: 0,
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
    setSource() { return Promise.resolve(); }
    destroy() {}
    play() { return Promise.resolve(); }
    pause() {}
    seek() {}
    snapshot() { return { currentTime: 0, duration: 8, ended: false }; }
  },
}));

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const DIRECT_PLAN = {
  type: "DIRECT" as const,
  url: "/api/v1/playback/sessions/web-client-decode/direct?expires=1900000000&signature=test",
};
const SERVER_PLAN = {
  type: "SERVER_HLS" as const,
  manifestUrl: "/api/v1/playback/sessions/web-client-decode/index.m3u8",
  tier: 4,
};

function matroskaSource(streams: Array<Record<string, unknown>>) {
  return {
    id: "source-client-decode",
    isDefault: true,
    sourceKind: "LOCAL_FILE",
    container: "matroska,webm",
    streams,
  };
}

describe("PlayerPage client decode gate", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;
  let warn: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    window.localStorage.clear();
    engineState.mkvEngines = 0;
    engineState.hevcEngines = 0;
    engineState.hlsEngines = 0;
    mockPlaybackBootstrap();
    warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    vi.spyOn(HTMLMediaElement.prototype, "load").mockImplementation(() => undefined);
    vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(() => undefined);
    vi.spyOn(HTMLMediaElement.prototype, "play").mockResolvedValue(undefined);
    vi.spyOn(api, "playback").mockResolvedValue({ positionTicks: 0, isPlayed: false, state: null, isPaused: false });
    vi.spyOn(api, "webPlaybackEvent").mockResolvedValue({ accepted: true, duplicate: false, stale: false });
    vi.spyOn(api, "stopWebPlaybackSession").mockResolvedValue(undefined);
    vi.spyOn(api, "progress").mockResolvedValue(undefined);
  });

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
    vi.useRealTimers();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

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
    // The fallback re-requests the session and then lazily imports the HLS engine, so give the
    // page a couple of macrotask rounds before asserting on the rendered surface.
    for (let round = 0; round < 3; round += 1) {
      await act(async () => {
        await new Promise((resolve) => setTimeout(resolve, 40));
      });
    }
  }

  function firstSessionDirectThenServerHls() {
    let calls = 0;
    return vi.spyOn(api, "createWebPlaybackSession").mockImplementation(async (_itemId, sourceId) => {
      calls += 1;
      return {
        sessionId: `web-${calls}`,
        playSessionId: `lux-web:web-${calls}`,
        sourceId,
        tier: calls === 1 ? 0 : 4,
        expiresAt: 1_900_000_000,
        plan: calls === 1 ? DIRECT_PLAN : SERVER_PLAN,
      };
    });
  }

  it("sends a 4K HEVC Matroska to server transcoding instead of the client remux", async () => {
    // Chromium-like browser: MSE decodes H.264 but not HEVC, and the element cannot open Matroska.
    vi.stubGlobal("MediaSource", { isTypeSupported: (mime: string) => mime.includes("avc1") });
    vi.stubGlobal("Worker", class Worker {});
    vi.stubGlobal("VideoEncoder", class VideoEncoder {
      static isConfigSupported() { return Promise.resolve({ supported: true }); }
    });
    const session = firstSessionDirectThenServerHls();
    vi.spyOn(api, "item").mockResolvedValue({
      id: "movie-4k-hevc",
      title: "4K HEVC",
      itemType: "MOVIE",
      mediaSources: [matroskaSource([
        { index: 0, type: "VIDEO", codec: "hevc", details: { Width: 3840, Height: 2160, PixelFormat: "yuv420p", Profile: "Main 10", Level: 153 } },
        { index: 1, type: "AUDIO", codec: "AAC" },
      ])],
    });

    await renderPlayer("movie-4k-hevc");

    expect(engineState.mkvEngines).toBe(0);
    expect(engineState.hevcEngines).toBe(0);
    expect(session).toHaveBeenCalledTimes(2);
    expect(session.mock.calls[1]?.[2]).toMatchObject({ directPlay: false });
    expect(engineState.hlsEngines).toBe(1);
    expect(container?.textContent).toContain("已切换为服务器转码");
    expect(warn).toHaveBeenCalledWith(expect.stringContaining("client-resolution-too-high"));
  });

  it("gates a 10-bit H.264 Matroska whose MSE only decodes 8-bit H.264", async () => {
    // Windows/Edge-like MSE: High profile yes, High 10 profile no, AC-3 not decodable.
    vi.stubGlobal("MediaSource", {
      isTypeSupported: (mime: string) => mime.includes("avc1.64002a") || mime.includes("avc1.640028"),
    });
    vi.stubGlobal("Worker", class Worker {});
    vi.stubGlobal("VideoEncoder", class VideoEncoder {
      static isConfigSupported() { return Promise.resolve({ supported: true }); }
    });
    const session = firstSessionDirectThenServerHls();
    vi.spyOn(api, "item").mockResolvedValue({
      id: "movie-h264-10bit",
      title: "H.264 10-bit",
      itemType: "MOVIE",
      mediaSources: [matroskaSource([
        { index: 0, type: "VIDEO", codec: "h264", details: { Width: 1920, Height: 1080, PixelFormat: "yuv420p10le" } },
        { index: 1, type: "AUDIO", codec: "AC3" },
      ])],
    });

    await renderPlayer("movie-h264-10bit");

    expect(engineState.mkvEngines).toBe(0);
    expect(session).toHaveBeenCalledTimes(2);
    expect(container?.textContent).toContain("已切换为服务器转码");
    expect(warn).toHaveBeenCalledWith(expect.stringContaining("client-codec-unsupported"));
  });

  it("leaves an 8-bit 1080p H.264 MP4 on native Direct Play", async () => {
    vi.stubGlobal("MediaSource", { isTypeSupported: () => true });
    vi.stubGlobal("Worker", class Worker {});
    const session = vi.spyOn(api, "createWebPlaybackSession").mockImplementation(async (_itemId, sourceId) => ({
      sessionId: "web-direct",
      playSessionId: "lux-web:web-direct",
      sourceId,
      tier: 0,
      expiresAt: 1_900_000_000,
      plan: DIRECT_PLAN,
    }));
    vi.spyOn(api, "item").mockResolvedValue({
      id: "movie-1080p",
      title: "1080p",
      itemType: "MOVIE",
      mediaSources: [{
        id: "source-1080p",
        isDefault: true,
        sourceKind: "LOCAL_FILE",
        container: "mp4",
        streams: [
          { index: 0, type: "VIDEO", codec: "h264", details: { Width: 1920, Height: 1080, PixelFormat: "yuv420p" } },
          { index: 1, type: "AUDIO", codec: "AAC" },
        ],
      }],
    });

    await renderPlayer("movie-1080p");

    expect(engineState.mkvEngines).toBe(0);
    expect(engineState.hevcEngines).toBe(0);
    expect(session).toHaveBeenCalledTimes(1);
    expect(container?.textContent).not.toContain("已切换为服务器转码");
    expect(warn).not.toHaveBeenCalledWith(expect.stringContaining("客户端解码"));
  });
});
