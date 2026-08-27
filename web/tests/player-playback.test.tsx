// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { PlayerPage } from "../src/features/player/PlayerPage";
import { api } from "../src/lib/api/client";
import { queryKeys } from "../src/lib/api/query-keys";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

function dispatchPointer(
  target: Element,
  type: "pointerdown" | "pointermove" | "pointerup" | "pointercancel",
  options: { pointerId: number; pointerType: string; clientX: number; clientY: number },
) {
  const event = new Event(type, { bubbles: true, cancelable: true });
  Object.defineProperties(event, {
    pointerId: { value: options.pointerId },
    pointerType: { value: options.pointerType },
    clientX: { value: options.clientX },
    clientY: { value: options.clientY },
  });
  target.dispatchEvent(event);
}

describe("PlayerPage playback synchronization", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;

  beforeEach(() => {
    vi.spyOn(api, "progress").mockResolvedValue(undefined);
    vi.spyOn(api, "createWebPlaybackSession").mockImplementation(async (_itemId, sourceId) => ({
      sessionId: `web-${sourceId}`,
      playSessionId: `lux-web:web-${sourceId}`,
      sourceId,
      tier: 0,
      expiresAt: 1_900_000_000,
      plan: {
        type: "DIRECT",
        url: `/api/v1/playback/sessions/web-${sourceId}/direct?expires=1900000000&signature=test`,
      },
    }));
    vi.spyOn(api, "webPlaybackEvent").mockResolvedValue({ accepted: true, duplicate: false, stale: false });
    vi.spyOn(api, "stopWebPlaybackSession").mockResolvedValue(undefined);
    vi.spyOn(HTMLMediaElement.prototype, "load").mockImplementation(() => undefined);
    vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(() => undefined);
  });

  afterEach(() => {
    vi.useRealTimers();
    if (root) act(() => root?.unmount());
    container?.remove();
    vi.restoreAllMocks();
  });

  it("resumes the shared position and reports play, pause, and stop states", async () => {
    vi.spyOn(api, "item").mockResolvedValue({
      id: "movie-1",
      title: "示例电影",
      itemType: "MOVIE",
      mediaSources: [{ id: "source-1", isDefault: true, durationTicks: 1_200_000_000 }],
    });
    vi.spyOn(api, "playback").mockResolvedValue({
      positionTicks: 420_000_000,
      isPlayed: false,
      state: "PAUSED",
      isPaused: true,
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const invalidateQueries = vi.spyOn(queryClient, "invalidateQueries");

    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/watch/movie-1"]}>
            <Routes>
              <Route path="watch/:itemId" element={<PlayerPage />} />
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const video = container.querySelector<HTMLVideoElement>("video");
    expect(video).not.toBeNull();
    Object.defineProperty(video, "duration", { configurable: true, value: 120 });
    Object.defineProperty(video, "currentTime", { configurable: true, writable: true, value: 0 });

    await act(async () => video?.dispatchEvent(new Event("loadedmetadata")));
    expect(video?.currentTime).toBe(42);

    if (!video) throw new Error("video element was not rendered");
    video.currentTime = 48;
    await act(async () => video.dispatchEvent(new Event("play")));
    await act(async () => video.dispatchEvent(new Event("pause")));
    await act(async () => video.dispatchEvent(new Event("ended")));
    await act(async () => window.dispatchEvent(new Event("pagehide")));

    expect(api.webPlaybackEvent).toHaveBeenNthCalledWith(
      1,
      "web-source-1",
      expect.objectContaining({
        sequence: 1,
        positionTicks: 480_000_000,
        durationTicks: 1_200_000_000,
        state: "PLAYING",
      }),
      false,
    );
    expect(api.webPlaybackEvent).toHaveBeenNthCalledWith(
      2,
      "web-source-1",
      expect.objectContaining({
        sequence: 2,
        positionTicks: 480_000_000,
        durationTicks: 1_200_000_000,
        state: "PAUSED",
      }),
      false,
    );
    expect(api.webPlaybackEvent).toHaveBeenNthCalledWith(
      3,
      "web-source-1",
      expect.objectContaining({
        sequence: 3,
        positionTicks: 480_000_000,
        durationTicks: 1_200_000_000,
        state: "STOPPED",
      }),
      false,
    );
    expect(api.webPlaybackEvent).toHaveBeenNthCalledWith(
      4,
      "web-source-1",
      expect.objectContaining({
        sequence: 4,
        positionTicks: 480_000_000,
        durationTicks: 1_200_000_000,
        state: "STOPPED",
      }),
      true,
    );
    expect(invalidateQueries).toHaveBeenCalledWith({ queryKey: queryKeys.home });
  });

  it("reports stopped when the player route is unmounted", async () => {
    vi.spyOn(api, "item").mockResolvedValue({
      id: "movie-2",
      title: "离开播放器测试",
      itemType: "MOVIE",
      mediaSources: [{ id: "source-2", isDefault: true, durationTicks: 1_200_000_000 }],
    });
    vi.spyOn(api, "playback").mockResolvedValue({
      positionTicks: 0,
      isPlayed: false,
      state: null,
      isPaused: false,
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/watch/movie-2"]}>
            <Routes>
              <Route path="watch/:itemId" element={<PlayerPage />} />
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const video = container.querySelector<HTMLVideoElement>("video");
    if (!video) throw new Error("video element was not rendered");
    Object.defineProperty(video, "duration", { configurable: true, value: 120 });
    Object.defineProperty(video, "currentTime", { configurable: true, writable: true, value: 30 });

    await act(async () => video.dispatchEvent(new Event("play")));
    await act(async () => {
      root?.unmount();
      root = undefined;
    });

    expect(api.webPlaybackEvent).toHaveBeenNthCalledWith(
      1,
      "web-source-2",
      expect.objectContaining({
        sequence: 1,
        positionTicks: 300_000_000,
        state: "PLAYING",
      }),
      false,
    );
    expect(api.webPlaybackEvent).toHaveBeenNthCalledWith(
      2,
      "web-source-2",
      expect.objectContaining({
        sequence: 2,
        positionTicks: 300_000_000,
        state: "STOPPED",
      }),
      false,
    );
  });

  it("uses the Emby proxy URL for path STRM and falls back to signed Lux direct play", async () => {
    vi.spyOn(api, "item").mockResolvedValue({
      id: "movie-proxy",
      title: "路径代理测试",
      itemType: "MOVIE",
      mediaSources: [{
        id: "source-proxy",
        isDefault: true,
        sourceKind: "STRM_URL",
        externalUrl: "/CloudNAS/115-122/media-AV/episode.mp4",
      }],
    });
    vi.spyOn(api, "playback").mockResolvedValue({
      positionTicks: 0,
      isPlayed: false,
      state: null,
      isPaused: false,
    });
    vi.mocked(api.createWebPlaybackSession).mockResolvedValueOnce({
      sessionId: "web-source-proxy",
      playSessionId: "lux-web:web-source-proxy",
      sourceId: "source-proxy",
      tier: 0,
      expiresAt: 1_900_000_000,
      plan: {
        type: "DIRECT",
        url: "/api/v1/playback/sessions/web-source-proxy/direct?expires=1900000000&signature=test",
        proxyUrl: "/Videos/movie-proxy/stream?MediaSourceId=source-proxy",
      },
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/watch/movie-proxy"]}>
            <Routes>
              <Route path="watch/:itemId" element={<PlayerPage />} />
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const video = container.querySelector<HTMLVideoElement>("video");
    expect(video?.getAttribute("src")).toBe("/Videos/movie-proxy/stream?MediaSourceId=source-proxy");
    await act(async () => video?.dispatchEvent(new Event("error")));
    expect(video?.getAttribute("src"))
      .toBe("/api/v1/playback/sessions/web-source-proxy/direct?expires=1900000000&signature=test");
  });

  it("stops the old session before creating a selected replacement source", async () => {
    const lifecycle: string[] = [];
    vi.mocked(api.createWebPlaybackSession).mockImplementation(async (_itemId, sourceId) => {
      lifecycle.push(`create:${sourceId}`);
      return {
        sessionId: `web-${sourceId}`,
        playSessionId: `lux-web:web-${sourceId}`,
        sourceId,
        tier: 0,
        expiresAt: 1_900_000_000,
        plan: {
          type: "DIRECT",
          url: `/api/v1/playback/sessions/web-${sourceId}/direct?expires=1900000000&signature=test`,
        },
      };
    });
    vi.mocked(api.stopWebPlaybackSession).mockImplementation(async (sessionId) => {
      lifecycle.push(`stop:${sessionId}`);
    });
    vi.spyOn(api, "item").mockResolvedValue({
      id: "movie-switch",
      title: "版本切换测试",
      itemType: "MOVIE",
      mediaSources: [
        { id: "source-1", isDefault: true, qualityLabel: "1080P" },
        { id: "source-2", qualityLabel: "4K" },
      ],
    });
    vi.spyOn(api, "playback").mockResolvedValue({
      positionTicks: 0,
      isPlayed: false,
      state: null,
      isPaused: false,
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/watch/movie-switch"]}>
            <Routes>
              <Route path="watch/:itemId" element={<PlayerPage />} />
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const sourceSelector = container.querySelector<HTMLSelectElement>('select[aria-label="选择播放源"]');
    expect(sourceSelector).not.toBeNull();
    if (!sourceSelector) throw new Error("source selector was not rendered");
    await act(async () => {
      sourceSelector.value = "source-2";
      sourceSelector.dispatchEvent(new Event("change", { bubbles: true }));
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    expect(lifecycle).toEqual(["create:source-1", "stop:web-source-1", "create:source-2"]);
  });

  it("waits for the direct session to stop before creating a server fallback", async () => {
    const lifecycle: string[] = [];
    let finishStop: (() => void) | undefined;
    vi.mocked(api.createWebPlaybackSession).mockImplementation(async (_itemId, sourceId) => {
      lifecycle.push(`create:${sourceId}`);
      return {
        sessionId: `web-${sourceId}`,
        playSessionId: `lux-web:web-${sourceId}`,
        sourceId,
        tier: 0,
        expiresAt: 1_900_000_000,
        plan: {
          type: "DIRECT",
          url: `/api/v1/playback/sessions/web-${sourceId}/direct?expires=1900000000&signature=test`,
        },
      };
    });
    vi.mocked(api.stopWebPlaybackSession).mockImplementation(async (sessionId) => {
      lifecycle.push(`stop:${sessionId}`);
      await new Promise<void>((resolve) => {
        finishStop = resolve;
      });
    });
    vi.spyOn(api, "item").mockResolvedValue({
      id: "movie-fallback-order",
      title: "回退会话顺序测试",
      itemType: "MOVIE",
      mediaSources: [{ id: "source-1", isDefault: true }],
    });
    vi.spyOn(api, "playback").mockResolvedValue({
      positionTicks: 0,
      isPlayed: false,
      state: null,
      isPaused: false,
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/watch/movie-fallback-order"]}>
            <Routes>
              <Route path="watch/:itemId" element={<PlayerPage />} />
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const video = container.querySelector<HTMLVideoElement>("video");
    if (!video) throw new Error("video element was not rendered");
    await act(async () => {
      video.dispatchEvent(new Event("error"));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(lifecycle).toEqual(["create:source-1", "stop:web-source-1"]);

    await act(async () => {
      finishStop?.();
      await new Promise((resolve) => setTimeout(resolve, 10));
    });
    expect(lifecycle).toEqual(["create:source-1", "stop:web-source-1", "create:source-1"]);
  });

  it("releases the web playback session when media reaches the end", async () => {
    vi.spyOn(api, "item").mockResolvedValue({
      id: "movie-3",
      title: "播放结束清理测试",
      itemType: "MOVIE",
      mediaSources: [{ id: "source-3", isDefault: true, durationTicks: 120_000_000 }],
    });
    vi.spyOn(api, "playback").mockResolvedValue({
      positionTicks: 0,
      isPlayed: false,
      state: null,
      isPaused: false,
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/watch/movie-3"]}>
            <Routes>
              <Route path="watch/:itemId" element={<PlayerPage />} />
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const video = container.querySelector<HTMLVideoElement>("video");
    if (!video) throw new Error("video element was not rendered");
    Object.defineProperty(video, "duration", { configurable: true, value: 12 });
    Object.defineProperty(video, "currentTime", { configurable: true, writable: true, value: 12 });

    await act(async () => video.dispatchEvent(new Event("play")));
    await act(async () => video.dispatchEvent(new Event("ended")));

    expect(api.stopWebPlaybackSession).toHaveBeenCalledWith("web-source-3", false);
  });

  it("keeps pointer controls active while seeking and uses pointer capture for the timeline", async () => {
    vi.spyOn(api, "item").mockResolvedValue({
      id: "movie-pointer",
      title: "手势交互测试",
      itemType: "MOVIE",
      mediaSources: [{ id: "source-pointer", isDefault: true, durationTicks: 1_000_000_000 }],
    });
    vi.spyOn(api, "playback").mockResolvedValue({
      positionTicks: 0,
      isPlayed: false,
      state: null,
      isPaused: false,
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/watch/movie-pointer"]}>
            <Routes>
              <Route path="watch/:itemId" element={<PlayerPage />} />
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const video = container.querySelector<HTMLVideoElement>("video");
    const player = container.querySelector<HTMLElement>(".lux-player-page");
    const timeline = container.querySelector<HTMLDivElement>('[aria-label="播放进度"]');
    if (!video || !player || !timeline) throw new Error("player controls were not rendered");
    Object.defineProperty(video, "duration", { configurable: true, value: 100 });
    Object.defineProperty(video, "currentTime", { configurable: true, writable: true, value: 0 });
    vi.spyOn(video, "getBoundingClientRect").mockReturnValue({
      left: 0,
      top: 0,
      width: 400,
      height: 200,
      right: 400,
      bottom: 200,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    });
    vi.spyOn(timeline, "getBoundingClientRect").mockReturnValue({
      left: 0,
      top: 0,
      width: 100,
      height: 22,
      right: 100,
      bottom: 22,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    });
    const capture = vi.fn();
    const release = vi.fn();
    Object.defineProperty(timeline, "setPointerCapture", { configurable: true, value: capture });
    Object.defineProperty(timeline, "hasPointerCapture", { configurable: true, value: () => true });
    Object.defineProperty(timeline, "releasePointerCapture", { configurable: true, value: release });

    await act(async () => video.dispatchEvent(new Event("loadedmetadata")));
    vi.useFakeTimers();
    await act(async () => video.dispatchEvent(new Event("play")));
    act(() => vi.advanceTimersByTime(3_000));
    expect(player.classList.contains("controls-hidden")).toBe(true);

    act(() => dispatchPointer(video, "pointerdown", { pointerId: 1, pointerType: "touch", clientX: 100, clientY: 100 }));
    act(() => dispatchPointer(video, "pointermove", { pointerId: 1, pointerType: "touch", clientX: 200, clientY: 100 }));
    expect(player.classList.contains("controls-visible")).toBe(true);
    act(() => vi.advanceTimersByTime(3_000));
    expect(player.classList.contains("controls-hidden")).toBe(false);
    act(() => dispatchPointer(video, "pointercancel", { pointerId: 1, pointerType: "touch", clientX: 200, clientY: 100 }));

    act(() => dispatchPointer(timeline, "pointerdown", { pointerId: 2, pointerType: "touch", clientX: 50, clientY: 10 }));
    act(() => dispatchPointer(timeline, "pointermove", { pointerId: 3, pointerType: "touch", clientX: 80, clientY: 10 }));
    act(() => dispatchPointer(timeline, "pointerup", { pointerId: 3, pointerType: "touch", clientX: 90, clientY: 10 }));
    act(() => dispatchPointer(timeline, "pointermove", { pointerId: 2, pointerType: "touch", clientX: 60, clientY: 10 }));
    act(() => dispatchPointer(timeline, "pointerup", { pointerId: 2, pointerType: "touch", clientX: 60, clientY: 10 }));

    expect(capture).toHaveBeenCalledWith(2);
    expect(release).toHaveBeenCalledWith(2);
    expect(video.currentTime).toBe(60);
    act(() => vi.advanceTimersByTime(3_000));
    expect(player.classList.contains("controls-hidden")).toBe(true);
  });
});
