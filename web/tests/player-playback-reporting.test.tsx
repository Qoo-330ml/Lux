// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../src/lib/api/client";
import { queryKeys } from "../src/lib/api/query-keys";
import {
  PLAYBACK_HEARTBEAT_INTERVAL_MS,
  PROGRESS_REPORT_INTERVAL_MS,
  usePlaybackReporting,
  type PlaybackReporting,
} from "../src/features/player/usePlaybackReporting";

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
  vi.spyOn(api, "webPlaybackEvent").mockResolvedValue({ accepted: true, duplicate: false, stale: false });
  vi.spyOn(api, "webPlaybackHeartbeat").mockResolvedValue({ sessionId: "session-1", expiresAt: 1_900_000_000 });
  vi.spyOn(api, "stopWebPlaybackSession").mockResolvedValue(undefined);
});

function fakeVideo(currentTime: number, duration: number | null) {
  return {
    currentTime,
    duration: duration ?? Number.NaN,
  } as unknown as HTMLVideoElement;
}

type HarnessRefs = {
  reporting: PlaybackReporting;
  video: { current: HTMLVideoElement | null };
  lastVideo: { current: HTMLVideoElement | null };
  sessionId: { current: string | null };
};

function mountHarness(initialSessionId: string | null, video: HTMLVideoElement, lastVideo: HTMLVideoElement) {
  const refs: HarnessRefs = {
    reporting: null as unknown as PlaybackReporting,
    video: { current: video },
    lastVideo: { current: lastVideo },
    sessionId: { current: initialSessionId },
  };
  function Harness() {
    refs.reporting = usePlaybackReporting({
      sessionIdRef: refs.sessionId,
      videoRef: refs.video,
      lastVideoRef: refs.lastVideo,
      sessionId: initialSessionId,
    });
    return null;
  }
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  const queryClient = new QueryClient();
  act(() => {
    root?.render(
      <QueryClientProvider client={queryClient}>
        <Harness />
      </QueryClientProvider>,
    );
  });
  return { refs, queryClient };
}

describe("usePlaybackReporting", () => {
  it("throttles non-forced reports to one per ten seconds", () => {
    vi.useFakeTimers();
    const video = fakeVideo(12.5, 120);
    const { refs } = mountHarness("session-1", video, video);

    act(() => {
      void refs.reporting.reportPlayback("PLAYING");
    });
    act(() => {
      void refs.reporting.reportPlayback("PLAYING");
    });
    expect(api.webPlaybackEvent).toHaveBeenCalledTimes(1);

    act(() => {
      vi.advanceTimersByTime(PROGRESS_REPORT_INTERVAL_MS - 1);
    });
    act(() => {
      void refs.reporting.reportPlayback("PLAYING");
    });
    expect(api.webPlaybackEvent).toHaveBeenCalledTimes(1);

    act(() => {
      vi.advanceTimersByTime(1);
    });
    act(() => {
      void refs.reporting.reportPlayback("PLAYING");
    });
    expect(api.webPlaybackEvent).toHaveBeenCalledTimes(2);

    act(() => {
      void refs.reporting.reportPlayback("PLAYING", true);
    });
    expect(api.webPlaybackEvent).toHaveBeenCalledTimes(3);
  });

  it("converts the video clock into ticks and drops an unknown duration", () => {
    vi.useFakeTimers();
    const video = fakeVideo(12.345, null);
    const { refs } = mountHarness("session-1", video, video);

    act(() => {
      void refs.reporting.reportPlayback("PLAYING", true);
    });

    expect(api.webPlaybackEvent).toHaveBeenCalledWith(
      "session-1",
      expect.objectContaining({
        state: "PLAYING",
        positionTicks: 123_450_000,
        durationTicks: null,
      }),
      false,
    );
  });

  it("ignores STOPPED before the first PLAYING and while no session exists", () => {
    vi.useFakeTimers();
    const video = fakeVideo(1, 120);
    const { refs } = mountHarness("session-1", video, video);

    act(() => {
      void refs.reporting.reportPlayback("STOPPED", true);
    });
    expect(api.webPlaybackEvent).not.toHaveBeenCalled();

    act(() => {
      refs.reporting.markStarted();
      refs.sessionId.current = null;
      void refs.reporting.reportPlayback("STOPPED", true);
    });
    expect(api.webPlaybackEvent).not.toHaveBeenCalled();
  });

  it("numbers accepted reports sequentially and keeps the event id format", () => {
    vi.useFakeTimers();
    const video = fakeVideo(3, 120);
    const { refs } = mountHarness("session-1", video, video);

    act(() => {
      refs.reporting.markStarted();
      void refs.reporting.reportPlayback("PLAYING", true);
      void refs.reporting.reportPlayback("PAUSED", true);
    });

    expect(api.webPlaybackEvent).toHaveBeenNthCalledWith(
      1,
      "session-1",
      expect.objectContaining({ sequence: 1, eventId: expect.stringMatching(/^web-session-1-1-\d+$/) }),
      false,
    );
    expect(api.webPlaybackEvent).toHaveBeenNthCalledWith(
      2,
      "session-1",
      expect.objectContaining({ sequence: 2 }),
      false,
    );

    act(() => {
      refs.reporting.resetSequence();
      void refs.reporting.reportPlayback("PAUSED", true);
    });
    expect(api.webPlaybackEvent).toHaveBeenNthCalledWith(
      3,
      "session-1",
      expect.objectContaining({ sequence: 1 }),
      false,
    );
  });

  it("removes the stopped session query and refreshes the home surface", async () => {
    vi.useFakeTimers();
    const video = fakeVideo(5, 120);
    const { refs, queryClient } = mountHarness("session-1", video, video);
    const removeQueries = vi.spyOn(queryClient, "removeQueries");
    const invalidateQueries = vi.spyOn(queryClient, "invalidateQueries");

    act(() => {
      refs.reporting.markStarted();
    });
    await act(async () => {
      await refs.reporting.reportPlayback("STOPPED", true);
    });

    expect(invalidateQueries).toHaveBeenCalledWith({ queryKey: queryKeys.home });
    expect(removeQueries).toHaveBeenCalledTimes(1);
    const filters = removeQueries.mock.calls[0][0] as unknown as {
      predicate: (query: { queryKey: readonly unknown[]; state: { data?: unknown } }) => boolean;
    };
    expect(filters.predicate({ queryKey: ["web-playback-session", "movie"], state: { data: { sessionId: "session-1" } } })).toBe(true);
    expect(filters.predicate({ queryKey: ["web-playback-session", "movie"], state: { data: { sessionId: "other" } } })).toBe(false);
    expect(filters.predicate({ queryKey: ["item", "movie"], state: { data: { sessionId: "session-1" } } })).toBe(false);
  });

  it("forwards keepalive to the stop request without refreshing the home surface", async () => {
    vi.useFakeTimers();
    const video = fakeVideo(5, 120);
    const { refs, queryClient } = mountHarness("session-1", video, video);
    const invalidateQueries = vi.spyOn(queryClient, "invalidateQueries");

    await act(async () => {
      await refs.reporting.stopActiveSession("session-1", true);
    });

    expect(api.stopWebPlaybackSession).toHaveBeenCalledWith("session-1", true);
    expect(invalidateQueries).not.toHaveBeenCalled();
    expect(queryClient.getQueryCache().getAll().length).toBe(0);
  });

  it("sends a heartbeat every minute while a session is open", () => {
    vi.useFakeTimers();
    const video = fakeVideo(0, 120);
    mountHarness("session-1", video, video);

    act(() => {
      vi.advanceTimersByTime(PLAYBACK_HEARTBEAT_INTERVAL_MS);
    });
    expect(api.webPlaybackHeartbeat).toHaveBeenCalledTimes(1);
    expect(api.webPlaybackHeartbeat).toHaveBeenCalledWith("session-1");

    act(() => {
      vi.advanceTimersByTime(PLAYBACK_HEARTBEAT_INTERVAL_MS);
    });
    expect(api.webPlaybackHeartbeat).toHaveBeenCalledTimes(2);
  });

  it("does not heartbeat without a session", () => {
    vi.useFakeTimers();
    const video = fakeVideo(0, 120);
    mountHarness(null, video, video);

    act(() => {
      vi.advanceTimersByTime(PLAYBACK_HEARTBEAT_INTERVAL_MS * 2);
    });
    expect(api.webPlaybackHeartbeat).not.toHaveBeenCalled();
  });

  it("reports with keepalive on pagehide and stops the session with keepalive", () => {
    const video = fakeVideo(30, 120);
    const { refs } = mountHarness("session-1", video, video);
    act(() => {
      refs.reporting.markStarted();
    });

    act(() => {
      window.dispatchEvent(new Event("pagehide"));
    });

    expect(api.webPlaybackEvent).toHaveBeenCalledWith(
      "session-1",
      expect.objectContaining({ state: "STOPPED" }),
      true,
    );
    expect(api.stopWebPlaybackSession).toHaveBeenCalledWith("session-1", true);
  });

  it("reports a final stop from the last video element on unmount", () => {
    const video = fakeVideo(10, 120);
    const lastVideo = fakeVideo(30, 120);
    const { refs } = mountHarness("session-1", video, lastVideo);
    act(() => {
      refs.reporting.markStarted();
    });

    act(() => {
      root?.unmount();
      root = undefined;
    });

    expect(api.webPlaybackEvent).toHaveBeenCalledWith(
      "session-1",
      expect.objectContaining({ state: "STOPPED", positionTicks: 300_000_000 }),
      false,
    );
    expect(api.stopWebPlaybackSession).toHaveBeenCalledWith("session-1", false);
  });
});
