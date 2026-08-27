// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { PlayerPage } from "../src/features/player/PlayerPage";
import { api } from "../src/lib/api/client";
import { shouldUseClientHevc } from "../src/features/player/playback-selection";

const fallbackState = vi.hoisted(() => ({
  assets: [] as Array<{ workerUrl: string; wasmUrl: string; wasmModuleUrl: string; wasmBinaryUrl: string }>,
  snapshotDuration: 8 as number | null,
}));

vi.mock("../src/features/player/playback-selection", () => ({
  shouldUseClientHevc: vi.fn().mockResolvedValue(true),
  shouldUseClientMkv: vi.fn().mockResolvedValue(false),
}));

vi.mock("../src/features/player/hevc-playback-engine", () => ({
  ClientHevcEngine: class MockClientHevcEngine {
    readonly kind = "client-hevc";
    readonly error = new Error("MSE SourceBuffer append failed");
    readonly performance = {
      mediaDurationMs: 8_000,
      processingDurationMs: 16_000,
      speedX: 0.5,
      realtime: false,
    };

    constructor(
      readonly element: HTMLVideoElement,
      readonly assets: { workerUrl: string; wasmUrl: string; wasmModuleUrl: string; wasmBinaryUrl: string },
    ) {
      fallbackState.assets.push(assets);
    }

    setSource() {
      return Promise.resolve();
    }

    destroy() {}
    play() { return this.element.play(); }
    pause() { this.element.pause(); }
    seek(seconds: number) { this.element.currentTime = seconds; }
    snapshot() { return { currentTime: 0, duration: fallbackState.snapshotDuration, ended: false }; }
  },
}));

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("PlayerPage client fallback status", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;

  beforeEach(() => {
    fallbackState.assets.length = 0;
    fallbackState.snapshotDuration = 8;
    vi.mocked(shouldUseClientHevc).mockResolvedValue(true);
    vi.spyOn(api, "item").mockResolvedValue({
      id: "movie-fallback",
      title: "4K fallback",
      itemType: "MOVIE",
      mediaSources: [{
        id: "source-fallback",
        isDefault: true,
        sourceKind: "LOCAL_FILE",
        container: "mp4",
        streams: [{ index: 0, type: "VIDEO", codec: "HEVC" }],
      }],
    });
    vi.spyOn(api, "playback").mockResolvedValue({
      positionTicks: 0,
      isPlayed: false,
      state: null,
      isPaused: false,
    });
    vi.spyOn(api, "createWebPlaybackSession").mockResolvedValue({
      sessionId: "web-fallback",
      playSessionId: "lux-web:web-fallback",
      sourceId: "source-fallback",
      tier: 0,
      expiresAt: 1_900_000_000,
      plan: {
        type: "DIRECT",
        url: "/api/v1/playback/sessions/web-fallback/direct?expires=1900000000&signature=test",
      },
    });
    vi.spyOn(api, "webPlaybackEvent").mockResolvedValue({ accepted: true, duplicate: false, stale: false });
    vi.spyOn(api, "stopWebPlaybackSession").mockResolvedValue(undefined);
    vi.spyOn(api, "progress").mockResolvedValue(undefined);
    vi.spyOn(HTMLMediaElement.prototype, "load").mockImplementation(() => undefined);
    vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(() => undefined);
  });

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
    vi.restoreAllMocks();
  });

  it("shows a clear degraded status when fallback throughput is below realtime", async () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/watch/movie-fallback"]}>
            <Routes>
              <Route path="watch/:itemId" element={<PlayerPage />} />
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 25));
    });
    expect(container?.textContent).toContain("客户端解码速度低于实时");
    expect(container?.textContent).toContain("使用原生客户端或降低清晰度");
    expect(fallbackState.assets[0]).toMatchObject({
      wasmUrl: "/hevc/hevc-decode.js",
    });
  });

  it("shows safe Lux guidance instead of the fallback engine reason", async () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/watch/movie-fallback"]}>
            <Routes>
              <Route path="watch/:itemId" element={<PlayerPage />} />
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 25));
    });

    const video = container.querySelector<HTMLVideoElement>("video");
    expect(video).not.toBeNull();
    await act(async () => video?.dispatchEvent(new Event("error")));

    expect(container?.textContent).toContain("播放器引擎失败");
    expect(container?.textContent).not.toContain("MSE SourceBuffer append failed");
  });

  it("updates the degraded status when fallback throughput arrives after first playback is ready", async () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/watch/movie-fallback"]}>
            <Routes>
              <Route path="watch/:itemId" element={<PlayerPage />} />
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 25));
    });

    const video = container.querySelector<HTMLVideoElement>("video");
    expect(video).not.toBeNull();
    await act(async () => video?.dispatchEvent(new CustomEvent("lux:playback-performance", {
      detail: { mediaDurationMs: 2_000, processingDurationMs: 8_000, speedX: 0.25, realtime: false },
    })));

    expect(container?.textContent).toContain("客户端解码速度低于实时");
    expect(container?.textContent).toContain("0.25×");
  });

  it("updates the seekable duration when fallback metadata finishes after canplay", async () => {
    fallbackState.snapshotDuration = null;
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/watch/movie-fallback"]}>
            <Routes>
              <Route path="watch/:itemId" element={<PlayerPage />} />
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 25));
    });

    const video = container.querySelector<HTMLVideoElement>("video");
    const timeline = container.querySelector<HTMLElement>("[role='slider'][aria-label='播放进度']");
    expect(video).not.toBeNull();
    expect(timeline?.getAttribute("aria-valuemax")).toBe("0");

    await act(async () => video?.dispatchEvent(new Event("canplay")));
    expect(timeline?.getAttribute("aria-valuemax")).toBe("0");

    fallbackState.snapshotDuration = 8;
    await act(async () => video?.dispatchEvent(new Event("durationchange")));

    expect(timeline?.getAttribute("aria-valuemax")).toBe("8");
  });
});
