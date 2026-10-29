// @vitest-environment jsdom

import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../src/lib/api/client";
import type { MediaSource } from "../src/lib/api/types";
import { usePlayerInfoPanel, type PlayerInfoPanel } from "../src/features/player/usePlayerInfoPanel";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let container: HTMLDivElement | undefined;
let root: Root | undefined;

afterEach(() => {
  vi.useRealTimers();
  if (root) act(() => root?.unmount());
  root = undefined;
  container?.remove();
  container = undefined;
  vi.restoreAllMocks();
});

beforeEach(() => {
  vi.spyOn(api, "webPlaybackStats").mockResolvedValue({
    sessionId: "session-1",
    ffmpeg: null,
    segments: [],
  } as never);
});

const SOURCE: MediaSource = { id: "source-1", isDefault: true };

function mountHarness(initialShowInfo: boolean, video: HTMLVideoElement | null = null) {
  const state: {
    info?: PlayerInfoPanel;
    setShowInfo: (value: boolean) => void;
  } = { setShowInfo: () => undefined };
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  function Harness() {
    const [showInfo, setShowInfo] = useState(initialShowInfo);
    state.setShowInfo = setShowInfo;
    state.info = usePlayerInfoPanel({
      playbackSessionId: "session-1",
      showInfo,
      playbackKey: "movie-1:source-1::0",
      playbackPlan: "DIRECT",
      tier: 0,
      source: SOURCE,
      engineRef: { current: null },
      videoRef: { current: video },
      lastVideoRef: { current: null },
      currentTime: 12,
      duration: 120,
      playbackRate: 1,
      volume: 1,
      isMuted: false,
      playing: true,
    });
    return null;
  }
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  act(() => {
    root?.render(
      <QueryClientProvider client={queryClient}>
        <Harness />
      </QueryClientProvider>,
    );
  });
  return state;
}

describe("usePlayerInfoPanel", () => {
  it("keeps the panel closed and polling off until it is opened", () => {
    vi.useFakeTimers();
    const state = mountHarness(false);

    act(() => {
      vi.advanceTimersByTime(5_000);
    });

    expect(api.webPlaybackStats).not.toHaveBeenCalled();
    expect(state.info?.infoGroups).toEqual([]);
  });

  it("polls the server and samples the client once per second while open", async () => {
    vi.useFakeTimers();
    const state = mountHarness(true);
    await act(async () => {
      await Promise.resolve();
    });
    expect(api.webPlaybackStats).toHaveBeenCalledTimes(1);

    act(() => {
      vi.advanceTimersByTime(1_000);
    });
    await act(async () => {
      await Promise.resolve();
    });
    expect(api.webPlaybackStats).toHaveBeenCalledTimes(2);
    expect(state.info?.infoGroups.length).toBeGreaterThan(0);
  });

  it("keeps sampling paused while the tab is hidden", async () => {
    vi.useFakeTimers();
    let hidden = false;
    Object.defineProperty(document, "hidden", { configurable: true, get: () => hidden });
    const quality = vi.fn(() => ({ totalVideoFrames: 120, droppedVideoFrames: 0 }) as unknown as VideoPlaybackQuality);
    const video = document.createElement("video");
    Object.defineProperty(video, "getVideoPlaybackQuality", { configurable: true, value: quality });

    mountHarness(true, video);
    await act(async () => {
      await Promise.resolve();
    });
    expect(quality).toHaveBeenCalledTimes(1);

    hidden = true;
    act(() => {
      vi.advanceTimersByTime(5_000);
    });
    expect(quality).toHaveBeenCalledTimes(1);

    hidden = false;
    act(() => {
      vi.advanceTimersByTime(1_000);
    });
    expect(quality).toHaveBeenCalledTimes(2);
  });

  it("stops both timers when the panel closes", async () => {
    vi.useFakeTimers();
    const state = mountHarness(true);
    await act(async () => {
      await Promise.resolve();
    });
    const callsWhileOpen = vi.mocked(api.webPlaybackStats).mock.calls.length;

    act(() => state.setShowInfo(false));
    await act(async () => {
      await Promise.resolve();
    });
    act(() => {
      vi.advanceTimersByTime(5_000);
    });
    await act(async () => {
      await Promise.resolve();
    });

    expect(vi.mocked(api.webPlaybackStats).mock.calls.length).toBe(callsWhileOpen);
    expect(state.info?.infoGroups).toEqual([]);
  });
});
