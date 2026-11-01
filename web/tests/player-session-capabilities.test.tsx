// @vitest-environment jsdom

/**
 * The playback session request carries the web capability claims the server decides on
 * (`docs/API.md`, Web 播放会话). These tests pin the two retry rules that keep a silent native track
 * from being copied again: a `forceAudioTranscode` retry never claims the audio track is copyable,
 * and the untrusted codecs (`ac-3`, ...) stop being copied once the first attempt already failed.
 */
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../src/lib/api/client";
import { usePlayerPlaybackSession } from "../src/features/player/usePlayerPlaybackSession";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const source = {
  id: "source-silent-audio",
  isDefault: true,
  sourceKind: "LOCAL_FILE",
  container: "mkv",
  streams: [
    { index: 0, type: "VIDEO", codec: "h264", details: { Width: 1920, Height: 1080 } },
    { index: 1, type: "AUDIO", codec: "aac" },
  ],
};

/** Renders only the session hook and returns its state, so the request payload can be inspected. */
function SessionHarness({ forceAudioTranscode }: { forceAudioTranscode: boolean }) {
  usePlayerPlaybackSession({
    itemId: "movie-silent-audio",
    requestedSourceId: source.id,
    requestedAudioStreamIndex: null,
    requestedSubtitleStreamIndex: null,
    requestedMaxBitrate: null,
    playbackAttempt: 1,
    forceAudioTranscode,
  });
  return null;
}

describe("playback session capabilities", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;

  beforeEach(() => {
    // A browser that claims both tracks, like Edge claiming AC-3 without the Dolby extension.
    vi.spyOn(HTMLMediaElement.prototype, "canPlayType").mockReturnValue("probably");
    vi.spyOn(api, "item").mockResolvedValue({
      id: "movie-silent-audio",
      title: "Silent audio",
      itemType: "MOVIE",
      mediaSources: [source],
    });
    vi.spyOn(api, "playback").mockResolvedValue({ positionTicks: 0, isPlayed: false, state: null, isPaused: false });
    vi.spyOn(api, "createWebPlaybackSession").mockResolvedValue({
      sessionId: "web-silent",
      playSessionId: "lux-web:web-silent",
      sourceId: source.id,
      tier: 2,
      expiresAt: 1_900_000_000,
      plan: { type: "SERVER_HLS", manifestUrl: "/api/v1/playback/sessions/web-silent/index.m3u8", tier: 2 },
    });
  });

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
    vi.restoreAllMocks();
  });

  async function renderSession(forceAudioTranscode: boolean) {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <SessionHarness forceAudioTranscode={forceAudioTranscode} />
        </QueryClientProvider>,
      );
    });
    for (let round = 0; round < 3; round += 1) {
      await act(async () => {
        await new Promise((resolve) => setTimeout(resolve, 20));
      });
    }
  }

  it("keeps claiming the audio copy for an ordinary retry", async () => {
    const session = vi.mocked(api.createWebPlaybackSession);
    await renderSession(false);

    expect(session).toHaveBeenCalledTimes(1);
    expect(session.mock.calls[0]?.[2]).toMatchObject({
      directPlay: false,
      videoCopyToFmp4: true,
      audioCopyToFmp4: true,
    });
  });

  it("declares the audio track non-copyable after a silent native audio track", async () => {
    const session = vi.mocked(api.createWebPlaybackSession);
    await renderSession(true);

    expect(session).toHaveBeenCalledTimes(1);
    expect(session.mock.calls[0]?.[2]).toMatchObject({
      directPlay: false,
      videoCopyToFmp4: true,
      audioCopyToFmp4: false,
    });
  });
});
