// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { PlayerPage } from "../src/features/player/PlayerPage";
import { api } from "../src/lib/api/client";
import { shouldUseClientHevc, shouldUseClientMkv } from "../src/features/player/playback-selection";
import { mockPlaybackBootstrap } from "./player-test-helpers";

const fallbackState = vi.hoisted(() => ({
  assets: [] as Array<{ workerUrl: string; wasmUrl: string; wasmModuleUrl: string; wasmBinaryUrl: string }>,
  mkvSources: [] as string[],
  captionSources: [] as string[],
  mkvFailure: false,
  snapshotDuration: 8 as number | null,
}));

vi.mock("../src/features/player/playback-selection", () => ({
  isRemoteHttpStrmSource: (source: { sourceKind?: string; externalUrl?: string }) =>
    source.sourceKind === "STRM_URL" && /^https?:\/\//i.test(source.externalUrl ?? ""),
  hasClientHevcRuntime: vi.fn().mockReturnValue(false),
  shouldUseClientHevc: vi.fn().mockResolvedValue(true),
  shouldUseClientMkv: vi.fn().mockResolvedValue(false),
}));

vi.mock("../src/features/player/chrome-caption-extension", () => ({
  ChromeCaptionExtension: class MockChromeCaptionExtension {
    constructor(private readonly options: { onReady?: () => void; onTracks?: (tracks: Array<{ id: string; label: string; language?: string; format: "srt"; isDefault: boolean; isForced: boolean; ordinal: number }>) => void }) {}

    start(source: string) {
      fallbackState.captionSources.push(source);
      queueMicrotask(() => {
        this.options.onTracks?.([{
          id: "mkv:42",
          label: "Chinese Simplified",
          language: "zho",
          format: "srt",
          isDefault: true,
          isForced: false,
          ordinal: 0,
        }]);
        this.options.onReady?.();
      });
    }

    setTime() {}
    stop() {}
  },
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

vi.mock("../src/features/player/mkv-playback-engine", () => ({
  ClientMkvEngine: class MockClientMkvEngine {
    readonly kind = "client-mkv";
    readonly error = null;
    readonly performance = null;

    constructor(readonly element: HTMLVideoElement) {}

    setSource(source: string) {
      fallbackState.mkvSources.push(source);
      if (fallbackState.mkvFailure) return Promise.reject(new Error("Range 读取失败"));
      return Promise.resolve();
    }

    destroy() {}
    play() { return this.element.play(); }
    pause() { this.element.pause(); }
    seek(seconds: number) { this.element.currentTime = seconds; }
    snapshot() { return { currentTime: 0, duration: 8, ended: false }; }
  },
}));

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("PlayerPage client fallback status", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;

  beforeEach(() => {
    mockPlaybackBootstrap();
    fallbackState.assets.length = 0;
    fallbackState.mkvSources.length = 0;
    fallbackState.captionSources.length = 0;
    fallbackState.mkvFailure = false;
    fallbackState.snapshotDuration = 8;
    vi.mocked(shouldUseClientHevc).mockResolvedValue(true);
    vi.mocked(shouldUseClientMkv).mockResolvedValue(false);
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

  it("keeps remote HTTP STRM on native direct playback instead of client fallback", async () => {
    vi.mocked(api.item).mockResolvedValue({
      id: "remote-hevc-strm",
      title: "远程 HEVC",
      itemType: "MOVIE",
      mediaSources: [{
        id: "remote-hevc-source",
        isDefault: true,
        sourceKind: "STRM_URL",
        externalUrl: "https://media.example.test/video.mp4",
        container: "mp4",
        streams: [{ index: 0, type: "VIDEO", codec: "HEVC" }],
      }],
    });
    vi.mocked(api.createWebPlaybackSession).mockResolvedValue({
      sessionId: "web-remote-hevc",
      playSessionId: "lux-web:web-remote-hevc",
      sourceId: "remote-hevc-source",
      tier: 0,
      expiresAt: 1_900_000_000,
      plan: {
        type: "DIRECT",
        url: "/api/v1/playback/sessions/web-remote-hevc/direct?expires=1900000000&signature=test",
        proxyUrl: "/Videos/remote-hevc-strm/stream.mp4?MediaSourceId=remote-hevc-source",
      },
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/watch/remote-hevc-strm"]}>
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

    expect(fallbackState.assets).toHaveLength(0);
    expect(shouldUseClientHevc).not.toHaveBeenCalled();
    expect(shouldUseClientMkv).not.toHaveBeenCalled();
    expect(container?.querySelector("video")?.getAttribute("src")).toBe(
      "/Videos/remote-hevc-strm/stream.mp4?MediaSourceId=remote-hevc-source",
    );
    expect(api.createWebPlaybackSession).toHaveBeenCalledWith(
      "remote-hevc-strm",
      "remote-hevc-source",
      expect.objectContaining({ directPlay: true }),
    );
  });

  it("keeps remote Matroska STRM native until an embedded caption is selected", async () => {
    vi.mocked(api.item).mockResolvedValue({
      id: "remote-mkv-strm",
      title: "远程 MKV",
      itemType: "MOVIE",
      mediaSources: [{
        id: "remote-mkv-source",
        isDefault: true,
        sourceKind: "STRM_URL",
        externalUrl: "https://media.example.test/video.mkv",
        container: "matroska,webm",
        streams: [
          { index: 0, type: "VIDEO", codec: "H264" },
          { index: 1, type: "AUDIO", codec: "AAC" },
          { index: 2, type: "SUBTITLE", codec: "SRT", isDefault: true },
        ],
      }],
    });
    vi.spyOn(api, "createWebPlaybackSession").mockResolvedValue({
      sessionId: "web-remote-mkv",
      playSessionId: "lux-web:web-remote-mkv",
      sourceId: "remote-mkv-source",
      tier: 0,
      expiresAt: 1_900_000_000,
      plan: {
        type: "DIRECT",
        url: "/api/v1/playback/sessions/web-remote-mkv/direct?expires=1900000000&signature=test",
        proxyUrl: "/Videos/remote-mkv-strm/stream.mkv?MediaSourceId=remote-mkv-source",
        rangeUrl: "/api/v1/playback/sessions/web-remote-mkv/range?expires=1900000000&signature=test",
      },
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/watch/remote-mkv-strm"]}>
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
    expect(video?.getAttribute("src")).toBe("/Videos/remote-mkv-strm/stream.mkv?MediaSourceId=remote-mkv-source");
    expect(shouldUseClientMkv).not.toHaveBeenCalled();
    expect(container?.textContent).not.toContain("播放器引擎失败");

    const settings = container?.querySelector<HTMLButtonElement>('[aria-label="播放器设置"]');
    expect(settings).not.toBeNull();
    await act(async () => settings?.click());
    const captionSelect = container?.querySelector<HTMLSelectElement>("#lux-player-caption-select");
    expect(captionSelect?.options[1]?.disabled).toBe(false);
    expect(container?.textContent).not.toContain("浏览器未暴露远程内嵌字幕");
    expect(shouldUseClientMkv).not.toHaveBeenCalled();
    expect(container?.querySelector("video")?.getAttribute("src")).toBe("/Videos/remote-mkv-strm/stream.mkv?MediaSourceId=remote-mkv-source");

    await act(async () => {
      if (!captionSelect) return;
      captionSelect.value = captionSelect.options[1]?.value ?? "";
      captionSelect.dispatchEvent(new Event("change", { bubbles: true }));
      await new Promise((resolve) => setTimeout(resolve, 100));
    });
    expect(shouldUseClientMkv).not.toHaveBeenCalled();
    expect(fallbackState.captionSources).toEqual([
      "http://localhost:3000/api/v1/playback/sessions/web-remote-mkv/direct?expires=1900000000&signature=test",
    ]);
    expect(container?.textContent).not.toContain("当前浏览器不支持远程字幕管线");
  });

  it("reads selected remote embedded captions through the direct URL extension bridge", async () => {
    vi.mocked(shouldUseClientMkv).mockResolvedValue(true);
    vi.mocked(api.item).mockResolvedValue({
      id: "remote-mkv-strm",
      title: "远程 MKV",
      itemType: "MOVIE",
      mediaSources: [{
        id: "remote-mkv-source",
        isDefault: true,
        sourceKind: "STRM_URL",
        externalUrl: "https://media.example.test/video.mkv",
        container: "matroska",
        streams: [
          { index: 0, type: "VIDEO", codec: "H264" },
          { index: 1, type: "AUDIO", codec: "AAC" },
          { index: 2, type: "SUBTITLE", codec: "SRT", isDefault: true },
        ],
      }],
    });
    vi.mocked(api.createWebPlaybackSession).mockResolvedValue({
      sessionId: "web-remote-mkv",
      playSessionId: "lux-web:web-remote-mkv",
      sourceId: "remote-mkv-source",
      tier: 0,
      expiresAt: 1_900_000_000,
      plan: {
        type: "DIRECT",
        url: "/api/v1/playback/sessions/web-remote-mkv/direct?expires=1900000000&signature=test",
        proxyUrl: "/Videos/remote-mkv-strm/stream.mkv?MediaSourceId=remote-mkv-source",
        rangeUrl: "/api/v1/playback/sessions/web-remote-mkv/range?expires=1900000000&signature=test",
      },
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/watch/remote-mkv-strm"]}>
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

    const settings = container.querySelector<HTMLButtonElement>('[aria-label="播放器设置"]');
    await act(async () => settings?.click());
    const captionSelect = container.querySelector<HTMLSelectElement>("#lux-player-caption-select");
    await act(async () => {
      if (!captionSelect) return;
      captionSelect.value = captionSelect.options[1]?.value ?? "";
      captionSelect.dispatchEvent(new Event("change", { bubbles: true }));
      await new Promise((resolve) => setTimeout(resolve, 100));
    });

    expect(fallbackState.captionSources).toEqual([
      "http://localhost:3000/api/v1/playback/sessions/web-remote-mkv/direct?expires=1900000000&signature=test",
    ]);
  });

  it("preserves remote playback position and playing state when entering the caption pipeline", async () => {
    vi.mocked(shouldUseClientMkv).mockResolvedValue(true);
    vi.mocked(api.item).mockResolvedValue({
      id: "remote-mkv-handoff",
      title: "远程 MKV 交接",
      itemType: "MOVIE",
      mediaSources: [{
        id: "remote-mkv-source",
        isDefault: true,
        sourceKind: "STRM_URL",
        externalUrl: "https://media.example.test/video.mkv",
        container: "matroska",
        streams: [
          { index: 0, type: "VIDEO", codec: "H264" },
          { index: 1, type: "AUDIO", codec: "AAC" },
          { index: 2, type: "SUBTITLE", codec: "SRT", isDefault: true },
        ],
      }],
    });
    vi.mocked(api.createWebPlaybackSession).mockResolvedValue({
      sessionId: "web-remote-mkv-handoff",
      playSessionId: "lux-web:web-remote-mkv-handoff",
      sourceId: "remote-mkv-source",
      tier: 0,
      expiresAt: 1_900_000_000,
      plan: {
        type: "DIRECT",
        url: "/api/v1/playback/sessions/web-remote-mkv-handoff/direct?expires=1900000000&signature=test",
        proxyUrl: "/Videos/remote-mkv-handoff/stream.mkv?MediaSourceId=remote-mkv-source",
        rangeUrl: "/api/v1/playback/sessions/web-remote-mkv-handoff/range?expires=1900000000&signature=test",
      },
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/watch/remote-mkv-handoff"]}>
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
    let currentTime = 42;
    let playing = true;
    Object.defineProperty(video, "currentTime", {
      configurable: true,
      get: () => currentTime,
      set: (value: number) => { currentTime = value; },
    });
    Object.defineProperty(video, "paused", {
      configurable: true,
      get: () => !playing,
    });
    vi.mocked(HTMLMediaElement.prototype.load).mockImplementation(() => {
      currentTime = 0;
    });
    const play = vi.spyOn(HTMLMediaElement.prototype, "play").mockImplementation(() => {
      playing = true;
      return Promise.resolve();
    });
    play.mockClear();

    const settings = container.querySelector<HTMLButtonElement>('[aria-label="播放器设置"]');
    await act(async () => settings?.click());
    const captionSelect = container.querySelector<HTMLSelectElement>("#lux-player-caption-select");
    await act(async () => {
      if (!captionSelect) return;
      captionSelect.value = captionSelect.options[1]?.value ?? "";
      captionSelect.dispatchEvent(new Event("change", { bubbles: true }));
      await new Promise((resolve) => setTimeout(resolve, 25));
    });

    expect(currentTime).toBe(42);
    expect(playing).toBe(true);
    expect(play).not.toHaveBeenCalled();
  });

  it("does not turn a remote caption pipeline failure into a playback-engine failure", async () => {
    fallbackState.mkvFailure = true;
    vi.mocked(shouldUseClientMkv).mockResolvedValue(true);
    vi.mocked(api.item).mockResolvedValue({
      id: "remote-mkv-strm",
      title: "远程 MKV",
      itemType: "MOVIE",
      mediaSources: [{
        id: "remote-mkv-source",
        isDefault: true,
        sourceKind: "STRM_URL",
        externalUrl: "https://media.example.test/video.mkv",
        container: "matroska",
        streams: [
          { index: 0, type: "VIDEO", codec: "H264" },
          { index: 1, type: "AUDIO", codec: "AAC" },
          { index: 2, type: "SUBTITLE", codec: "SRT", isDefault: true },
        ],
      }],
    });
    vi.mocked(api.createWebPlaybackSession).mockResolvedValue({
      sessionId: "web-remote-mkv",
      playSessionId: "lux-web:web-remote-mkv",
      sourceId: "remote-mkv-source",
      tier: 0,
      expiresAt: 1_900_000_000,
      plan: {
        type: "DIRECT",
        url: "/api/v1/playback/sessions/web-remote-mkv/direct?expires=1900000000&signature=test",
        proxyUrl: "/Videos/remote-mkv-strm/stream.mkv?MediaSourceId=remote-mkv-source",
        rangeUrl: "/api/v1/playback/sessions/web-remote-mkv/range?expires=1900000000&signature=test",
      },
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/watch/remote-mkv-strm"]}>
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
    const settings = container.querySelector<HTMLButtonElement>('[aria-label="播放器设置"]');
    await act(async () => settings?.click());
    const captionSelect = container.querySelector<HTMLSelectElement>("#lux-player-caption-select");
    await act(async () => {
      if (!captionSelect) return;
      captionSelect.value = captionSelect.options[1]?.value ?? "";
      captionSelect.dispatchEvent(new Event("change", { bubbles: true }));
      await new Promise((resolve) => setTimeout(resolve, 25));
    });

    expect(container.textContent).not.toContain("播放器引擎失败");
    expect(fallbackState.mkvSources).toHaveLength(0);
    expect(container.textContent).not.toContain("播放器引擎失败");
    expect(api.stopWebPlaybackSession).not.toHaveBeenCalled();
  });

  it("keeps native playback and the selected option when the remote codec pair cannot enter MSE", async () => {
    vi.mocked(api.item).mockResolvedValue({
      id: "remote-mkv-eac3",
      title: "远程 HEVC E-AC-3",
      itemType: "MOVIE",
      mediaSources: [{
        id: "remote-mkv-eac3-source",
        isDefault: true,
        sourceKind: "STRM_URL",
        externalUrl: "https://media.example.test/video.mkv",
        container: "mkv",
        streams: [
          { index: 0, type: "VIDEO", codec: "HEVC" },
          { index: 1, type: "AUDIO", codec: "EAC3" },
          { index: 2, type: "SUBTITLE", codec: "ASS" },
        ],
      }],
    });
    vi.mocked(api.createWebPlaybackSession).mockResolvedValue({
      sessionId: "web-remote-mkv-eac3",
      playSessionId: "lux-web:web-remote-mkv-eac3",
      sourceId: "remote-mkv-eac3-source",
      tier: 0,
      expiresAt: 1_900_000_000,
      plan: {
        type: "DIRECT",
        url: "/api/v1/playback/sessions/web-remote-mkv-eac3/direct?expires=1900000000&signature=test",
        proxyUrl: "/Videos/remote-mkv-eac3/stream.mkv?MediaSourceId=remote-mkv-eac3-source",
        rangeUrl: "/api/v1/playback/sessions/web-remote-mkv-eac3/range?expires=1900000000&signature=test",
      },
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/watch/remote-mkv-eac3"]}>
            <Routes><Route path="watch/:itemId" element={<PlayerPage />} /></Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
      await new Promise((resolve) => setTimeout(resolve, 25));
    });
    const settings = container.querySelector<HTMLButtonElement>('[aria-label="播放器设置"]');
    await act(async () => settings?.click());
    const captionSelect = container.querySelector<HTMLSelectElement>("#lux-player-caption-select");
    await act(async () => {
      if (!captionSelect) return;
      captionSelect.value = captionSelect.options[1]?.value ?? "";
      captionSelect.dispatchEvent(new Event("change", { bubbles: true }));
      await new Promise((resolve) => setTimeout(resolve, 25));
    });

    expect(container.textContent).not.toContain("播放器引擎失败");
    expect(shouldUseClientMkv).not.toHaveBeenCalled();
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
