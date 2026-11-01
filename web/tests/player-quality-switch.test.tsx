// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import { PlayerPage } from "../src/features/player/PlayerPage";
import { api } from "../src/lib/api/client";
import { queryKeys } from "../src/lib/api/query-keys";
import { chooseLuxOptionByValue, luxOptions, luxSelectValue } from "./lux-select-helpers";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

function CurrentLocation() {
  const location = useLocation();
  return <output data-testid="current-location">{`${location.pathname}${location.search}`}</output>;
}

const item = {
  id: "movie-quality",
  title: "画质切换测试",
  itemType: "MOVIE" as const,
  mediaSources: [{
    id: "source-quality",
    isDefault: true,
    sourceKind: "LOCAL_FILE",
    durationTicks: 1_200_000_000,
    bitrate: 12_000_000,
    streams: [
      { index: 0, type: "VIDEO", codec: "hevc" },
      {
        index: 1,
        type: "AUDIO",
        language: "eng",
        codec: "aac",
        isDefault: true,
        details: { Channels: 2 },
      },
      {
        index: 2,
        type: "AUDIO",
        language: "chi",
        codec: "eac3",
        details: { Channels: 6 },
      },
      { index: 7, type: "SUBTITLE", language: "chi", codec: "srt" },
    ],
  }, {
    id: "source-quality-b",
    sourceKind: "LOCAL_FILE",
    durationTicks: 1_200_000_000,
    bitrate: 6_000_000,
    streams: [
      { index: 0, type: "VIDEO", codec: "h264" },
      { index: 1, type: "AUDIO", codec: "aac" },
    ],
  }],
};

type SessionSelection = { audio?: number; subtitle?: number; maxBitrate?: number };

describe("PlayerPage quality switching", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;
  let bootstrapSelections: SessionSelection[] = [];
  let lifecycle: string[] = [];
  let queryClient: QueryClient;
  let play: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    window.localStorage.clear();
    bootstrapSelections = [];
    lifecycle = [];
    vi.spyOn(api, "item").mockResolvedValue(item);
    vi.spyOn(api, "playback").mockResolvedValue({
      positionTicks: 0,
      isPlayed: false,
      state: null,
      isPaused: false,
    });
    vi.spyOn(api, "createWebPlaybackBootstrap").mockImplementation(async (
      itemId,
      requestedSourceId,
      capabilities,
      signal,
      selection,
    ) => {
      bootstrapSelections.push({
        audio: selection?.audioStreamIndex,
        subtitle: selection?.subtitleStreamIndex,
        maxBitrate: selection?.maxBitrate,
      });
      const mediaItem = await api.item(itemId);
      const playback = await api.playback(itemId);
      const source = requestedSourceId
        ? mediaItem.mediaSources?.find((entry) => entry.id === requestedSourceId)
        : mediaItem.mediaSources?.[0];
      if (!source) throw new Error("test item has no playback source");
      const session = await api.createWebPlaybackSession(
        itemId,
        source.id,
        capabilities,
        signal,
        selection,
      );
      return { item: mediaItem, playback, session };
    });
    vi.spyOn(api, "createWebPlaybackSession").mockImplementation(async (
      _itemId,
      sourceId,
      _capabilities,
      _signal,
      selection,
    ) => {
      const variant = `${selection?.audioStreamIndex ?? "default"}-${selection?.maxBitrate ?? "auto"}`;
      lifecycle.push(`create:${variant}`);
      const maxBitrate = selection?.maxBitrate ?? null;
      // 真实后端对本地文件带 audioStreamIndex 的请求关闭直连，改用服务端会话固定音轨。
      const audioSelected = selection?.audioStreamIndex !== undefined;
      return {
        sessionId: `web-${variant}`,
        playSessionId: `lux-web:web-${variant}`,
        sourceId,
        tier: audioSelected ? 2 : 0,
        expiresAt: 1_900_000_000,
        plan: audioSelected
          ? {
            type: "SERVER_HLS" as const,
            manifestUrl: `/api/v1/playback/sessions/web-${variant}/manifest.m3u8?expires=1900000000&signature=test`,
            tier: 2,
          }
          : {
            type: "DIRECT" as const,
            url: `/api/v1/playback/sessions/web-${variant}/direct?expires=1900000000&signature=test`,
          },
        bitrate: {
          sourceBitrate: 12_000_000,
          maxBitrate,
          targetBitrate: maxBitrate,
          limited: maxBitrate !== null,
        },
      };
    });
    vi.spyOn(api, "stopWebPlaybackSession").mockImplementation(async (sessionId) => {
      lifecycle.push(`stop:${sessionId}`);
    });
    vi.spyOn(api, "webPlaybackEvent").mockResolvedValue({ accepted: true, duplicate: false, stale: false });
    vi.spyOn(HTMLMediaElement.prototype, "load").mockImplementation(() => undefined);
    vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(() => undefined);
    // jsdom 没有 MSE，按原生 HLS 可播放处理，让带音轨参数的服务端会话正常加载 manifest。
    vi.spyOn(HTMLMediaElement.prototype, "canPlayType")
      .mockImplementation((type) => (type === "application/vnd.apple.mpegurl" ? "maybe" : ""));
    play = vi.spyOn(HTMLMediaElement.prototype, "play").mockImplementation(async () => undefined);
  });

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  async function renderPlayer(initialEntry: string) {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={[initialEntry]}>
            <Routes>
              <Route path="watch/:itemId" element={<><PlayerPage /><CurrentLocation /></>} />
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
  }

  function primeVideo(video: HTMLVideoElement) {
    Object.defineProperty(video, "duration", { configurable: true, value: 120 });
    Object.defineProperty(video, "currentTime", { configurable: true, writable: true, value: 0 });
  }

  async function openSettings() {
    const settings = container?.querySelector<HTMLButtonElement>('[aria-label="播放器设置"]');
    if (!settings) throw new Error("settings control was not rendered");
    await act(async () => settings.click());
  }

  async function settleReplacement(srcPart: string) {
    for (let attempt = 0; attempt < 50; attempt += 1) {
      const video = container?.querySelector<HTMLVideoElement>("video");
      if (video?.getAttribute("src")?.includes(srcPart)) {
        primeVideo(video);
        await act(async () => video.dispatchEvent(new Event("canplay")));
        await act(async () => {
          await new Promise((resolve) => setTimeout(resolve, 0));
        });
        return video;
      }
      await act(async () => {
        await new Promise((resolve) => setTimeout(resolve, 5));
      });
    }
    throw new Error(`replacement video for ${srcPart} was not rendered`);
  }

  it("rebuilds the session at the current position when a ceiling is chosen", async () => {
    await renderPlayer("/watch/movie-quality?subtitleStreamIndex=7");

    const firstVideo = container?.querySelector<HTMLVideoElement>("video");
    if (!firstVideo) throw new Error("video element was not rendered");
    primeVideo(firstVideo);

    await openSettings();
    const qualityOptionsSnapshot = await luxOptions(container!, "选择画质");
    expect(qualityOptionsSnapshot.map((option) => option.value))
      .toEqual(["", "8000000", "4000000", "2000000", "1000000"]);
    expect(qualityOptionsSnapshot.map((option) => option.label))
      .toEqual(["原画（自动）", "8 Mbps", "4 Mbps", "2 Mbps", "1 Mbps"]);
    expect(luxSelectValue(container!, "选择画质")).toBe("");
    expect(container?.textContent).toContain("原画（自动）");

    const speed = Array.from(container!.querySelectorAll<HTMLButtonElement>("button"))
      .find((button) => button.textContent?.trim() === "1.5x");
    if (!speed) throw new Error("1.5x speed control was not rendered");
    await act(async () => speed.click());
    expect(firstVideo.playbackRate).toBe(1.5);

    firstVideo.currentTime = 48;
    firstVideo.volume = 0.4;
    firstVideo.muted = true;
    await act(async () => firstVideo.dispatchEvent(new Event("play")));

    await chooseLuxOptionByValue(container!, "选择画质", "4000000");
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 20));
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(container?.querySelector('[data-testid="current-location"]')?.textContent)
      .toBe("/watch/movie-quality?subtitleStreamIndex=7&maxBitrate=4000000");
    expect(bootstrapSelections).toEqual([
      { audio: undefined, subtitle: 7, maxBitrate: undefined },
      { audio: undefined, subtitle: 7, maxBitrate: 4_000_000 },
    ]);
    expect(lifecycle).toEqual([
      "create:default-auto",
      "stop:web-default-auto",
      "create:default-4000000",
    ]);
    expect(api.webPlaybackEvent).toHaveBeenCalledWith(
      "web-default-auto",
      expect.objectContaining({ state: "PLAYING", positionTicks: 480_000_000 }),
      false,
    );

    const replacementVideo = await settleReplacement("web-default-4000000");
    expect(replacementVideo.currentTime).toBe(48);
    expect(replacementVideo.playbackRate).toBe(1.5);
    expect(replacementVideo.volume).toBe(0.4);
    expect(replacementVideo.muted).toBe(true);
    expect(play).toHaveBeenCalled();
    expect(luxSelectValue(container!, "选择画质")).toBe("4000000");
    expect(container?.textContent).toContain("已限制到 4 Mbps");
  });

  it("keeps the audio selection when the quality changes and vice versa", async () => {
    await renderPlayer("/watch/movie-quality?audioStreamIndex=2");
    await openSettings();
    expect(luxSelectValue(container!, "选择音轨")).toBe("2");

    await chooseLuxOptionByValue(container!, "选择画质", "2000000");
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 20));
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(container?.querySelector('[data-testid="current-location"]')?.textContent)
      .toContain("audioStreamIndex=2");
    expect(container?.querySelector('[data-testid="current-location"]')?.textContent)
      .toContain("maxBitrate=2000000");
    expect(bootstrapSelections.at(-1)).toEqual({ audio: 2, subtitle: undefined, maxBitrate: 2_000_000 });
    const cappedVideo = await settleReplacement("web-2-2000000");
    expect(luxSelectValue(container!, "选择音轨")).toBe("2");
    expect(luxSelectValue(container!, "选择画质")).toBe("2000000");

    cappedVideo.currentTime = 12;
    await act(async () => chooseLuxOptionByValue(container!, "选择音轨", "1"));
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 20));
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(container?.querySelector('[data-testid="current-location"]')?.textContent)
      .toBe("/watch/movie-quality?maxBitrate=2000000");
    // 服务端会话已按 0:a:0 复用第一条音轨，切回它时参数被删掉。
    expect(bootstrapSelections.at(-1)).toEqual({ audio: undefined, subtitle: undefined, maxBitrate: 2_000_000 });
    const audioVideo = await settleReplacement("web-default-2000000");
    expect(audioVideo.currentTime).toBe(12);
    expect(luxSelectValue(container!, "选择画质")).toBe("2000000");
  });

  it("restores the saved ceiling from the URL after a refresh", async () => {
    await renderPlayer("/watch/movie-quality?maxBitrate=2000000");

    expect(bootstrapSelections).toEqual([
      { audio: undefined, subtitle: undefined, maxBitrate: 2_000_000 },
    ]);
    await openSettings();
    expect(luxSelectValue(container!, "选择画质")).toBe("2000000");
    expect(container?.textContent).toContain("已限制到 2 Mbps");
    const optionsSnapshot = await luxOptions(container!, "选择画质");
    // 12 Mbps source: the 20 Mbps tier is never offered.
    expect(optionsSnapshot.map((option) => option.value)).not.toContain("20000000");
  });

  it("rolls back to the previous ceiling when the replacement session fails", async () => {
    vi.mocked(api.createWebPlaybackSession).mockImplementation(async (
      _itemId,
      sourceId,
      _capabilities,
      _signal,
      selection,
    ) => {
      if (selection?.maxBitrate === 4_000_000) throw new Error("quality session failed");
      const maxBitrate = selection?.maxBitrate ?? null;
      return {
        sessionId: "web-default-auto",
        playSessionId: "lux-web:web-default-auto",
        sourceId,
        tier: 0,
        expiresAt: 1_900_000_000,
        plan: {
          type: "DIRECT" as const,
          url: "/api/v1/playback/sessions/web-default-auto/direct?expires=1900000000&signature=test",
        },
        bitrate: {
          sourceBitrate: 12_000_000,
          maxBitrate,
          targetBitrate: maxBitrate,
          limited: maxBitrate !== null,
        },
      };
    });
    await renderPlayer("/watch/movie-quality");
    await openSettings();

    await chooseLuxOptionByValue(container!, "选择画质", "4000000");
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 30));
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(container?.querySelector('[data-testid="current-location"]')?.textContent)
      .toBe("/watch/movie-quality");
    expect(container?.textContent).toContain("画质切换失败，已恢复原画。");
    const select = container?.querySelector<HTMLElement>('[aria-label="选择画质"]');
    expect(select).not.toBeNull();
    expect(select?.getAttribute("disabled")).toBeNull();
    expect(vi.mocked(api.createWebPlaybackBootstrap).mock.calls
      .filter((call) => call[4]?.maxBitrate === 4_000_000)).toHaveLength(1);
  });

  it("keeps the playing variant cached when the URL uses another notation", async () => {
    // `1e7` parses to 10 Mbps, but the query key slot is "10000000". Treating
    // the raw text as the current variant would drop the session that is on
    // screen and look like a source change.
    await renderPlayer("/watch/movie-quality?maxBitrate=1e7");
    expect(bootstrapSelections).toEqual([
      { audio: undefined, subtitle: undefined, maxBitrate: 10_000_000 },
    ]);
    const otherVariant = queryKeys.playbackBootstrap(
      "movie-quality",
      undefined,
      0,
      undefined,
      undefined,
      8_000_000,
    );
    queryClient.setQueryData(otherVariant, { marker: "other" });

    await openSettings();
    await chooseLuxOptionByValue(container!, "选择画质", "4000000");
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 20));
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const cachedVariants = queryClient
      .getQueryCache()
      .findAll({ queryKey: ["playback-bootstrap", "movie-quality"] })
      .map((query) => query.queryKey[5]);
    expect(cachedVariants).not.toContain("8000000");
    // The variant that was playing is not "some other variant": it must survive
    // the switch, otherwise the replaced session is recreated behind the scenes.
    expect(cachedVariants).toContain("10000000");
    expect(bootstrapSelections.filter((entry) => entry.maxBitrate === 10_000_000)).toHaveLength(1);
    expect(container?.querySelector('[data-testid="current-location"]')?.textContent)
      .toBe("/watch/movie-quality?maxBitrate=4000000");
  });

  it("rolls back to the canonical ceiling when the URL used padding", async () => {
    vi.mocked(api.createWebPlaybackSession).mockImplementation(async (
      _itemId,
      sourceId,
      _capabilities,
      _signal,
      selection,
    ) => {
      if (selection?.maxBitrate === 4_000_000) throw new Error("quality session failed");
      return {
        sessionId: "web-default-auto",
        playSessionId: "lux-web:web-default-auto",
        sourceId,
        tier: 0,
        expiresAt: 1_900_000_000,
        plan: {
          type: "DIRECT" as const,
          url: "/api/v1/playback/sessions/web-default-auto/direct?expires=1900000000&signature=test",
        },
        bitrate: {
          sourceBitrate: 12_000_000,
          maxBitrate: 100_000,
          targetBitrate: 100_000,
          limited: true,
        },
      };
    });
    await renderPlayer("/watch/movie-quality?maxBitrate=%20100000%20");
    expect(bootstrapSelections).toEqual([
      { audio: undefined, subtitle: undefined, maxBitrate: 100_000 },
    ]);

    await openSettings();
    await chooseLuxOptionByValue(container!, "选择画质", "4000000");
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 30));
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(container?.querySelector('[data-testid="current-location"]')?.textContent)
      .toBe("/watch/movie-quality?maxBitrate=100000");
    expect(container?.textContent).toContain("画质切换失败，已恢复原画。");
  });

  it("keeps the ceiling when switching the played version", async () => {
    await renderPlayer(
      "/watch/movie-quality?audioStreamIndex=2&subtitleStreamIndex=7&maxBitrate=2000000",
    );
    await openSettings();
    await chooseLuxOptionByValue(container!, "选择播放版本", "source-quality-b");

    expect(container?.querySelector('[data-testid="current-location"]')?.textContent)
      .toBe("/watch/movie-quality?sourceId=source-quality-b&maxBitrate=2000000");
  });

  it("does not offer a ceiling for STRM sources", async () => {
    vi.mocked(api.item).mockResolvedValue({
      ...item,
      mediaSources: [{
        ...item.mediaSources[0],
        sourceKind: "STRM_URL",
      }],
    });
    await renderPlayer("/watch/movie-quality");

    await openSettings();
    expect(container?.querySelector('[aria-label="选择画质"]')).toBeNull();
  });
});
