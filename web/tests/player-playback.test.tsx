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

describe("PlayerPage playback synchronization", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;

  beforeEach(() => {
    vi.spyOn(api, "progress").mockResolvedValue(undefined);
  });

  afterEach(() => {
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

    expect(api.progress).toHaveBeenNthCalledWith(
      1,
      "movie-1",
      480_000_000,
      1_200_000_000,
      "PLAYING",
      false,
    );
    expect(api.progress).toHaveBeenNthCalledWith(
      2,
      "movie-1",
      480_000_000,
      1_200_000_000,
      "PAUSED",
      false,
    );
    expect(api.progress).toHaveBeenNthCalledWith(
      3,
      "movie-1",
      480_000_000,
      1_200_000_000,
      "STOPPED",
      false,
    );
    expect(api.progress).toHaveBeenNthCalledWith(
      4,
      "movie-1",
      480_000_000,
      1_200_000_000,
      "STOPPED",
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

    expect(api.progress).toHaveBeenNthCalledWith(
      1,
      "movie-2",
      300_000_000,
      1_200_000_000,
      "PLAYING",
      false,
    );
    expect(api.progress).toHaveBeenNthCalledWith(
      2,
      "movie-2",
      300_000_000,
      1_200_000_000,
      "STOPPED",
      false,
    );
  });
});
