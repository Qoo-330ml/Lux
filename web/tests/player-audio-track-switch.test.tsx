// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import { PlayerPage } from "../src/features/player/PlayerPage";
import { api } from "../src/lib/api/client";
import { chooseLuxOptionByValue, luxOptions, luxSelectValue } from "./lux-select-helpers";
import { speedButton, speedLabel, trackMenuButton } from "./player-test-helpers";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

function CurrentLocation() {
  const location = useLocation();
  return <output data-testid="current-location">{`${location.pathname}${location.search}`}</output>;
}

const item = {
  id: "movie-audio",
  title: "音轨切换测试",
  itemType: "MOVIE" as const,
  mediaSources: [{
    id: "source-audio",
    isDefault: true,
    sourceKind: "LOCAL_FILE",
    durationTicks: 1_200_000_000,
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
  }],
};

type BootstrapSelection = { audio?: number; subtitle?: number };

describe("PlayerPage audio track switching", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;
  let bootstrapSelections: BootstrapSelection[] = [];
  let lifecycle: string[] = [];
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
      const audio = selection?.audioStreamIndex ?? "default";
      const sessionId = `web-${sourceId}-${audio}`;
      lifecycle.push(`create:${sourceId}:${audio}`);
      // 真实后端对本地文件带 audioStreamIndex 的请求关闭直连，改用服务端会话固定音轨。
      const plan = selection?.audioStreamIndex === undefined
        ? {
          type: "DIRECT" as const,
          url: `/api/v1/playback/sessions/${sessionId}/direct?expires=1900000000&signature=test`,
        }
        : {
          type: "SERVER_HLS" as const,
          manifestUrl: `/api/v1/playback/sessions/${sessionId}/manifest.m3u8?expires=1900000000&signature=test`,
          tier: 2,
        };
      return {
        sessionId,
        playSessionId: `lux-web:${sessionId}`,
        sourceId,
        tier: selection?.audioStreamIndex === undefined ? 0 : 2,
        expiresAt: 1_900_000_000,
        plan,
      };
    });
    vi.spyOn(api, "stopWebPlaybackSession").mockImplementation(async (sessionId) => {
      lifecycle.push(`stop:${sessionId}`);
    });
    vi.spyOn(api, "webPlaybackEvent").mockResolvedValue({ accepted: true, duplicate: false, stale: false });
    vi.spyOn(HTMLMediaElement.prototype, "load").mockImplementation(() => undefined);
    vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(() => undefined);
    // jsdom 没有 MSE，按原生 HLS 可播放处理，让服务端会话走正常的 manifest 加载路径。
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
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
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

  it("rebuilds the session at the current position while keeping speed, state and captions", async () => {
    await renderPlayer("/watch/movie-audio?subtitleStreamIndex=7");

    const firstVideo = container?.querySelector<HTMLVideoElement>("video");
    if (!firstVideo) throw new Error("video element was not rendered");
    primeVideo(firstVideo);

    await act(async () => trackMenuButton(container!, "音轨").click());

    const audioOptions = await luxOptions(container!, "选择音轨");
    expect(audioOptions.map((option) => option.value)).toEqual(["1", "2"]);
    expect(audioOptions[0]?.label).toContain("英语");
    expect(audioOptions[1]?.label).toContain("中文");
    expect(audioOptions[1]?.label).toContain("5.1");
    expect(luxSelectValue(container!, "选择音轨")).toBe("1");
    await act(async () => trackMenuButton(container!, "字幕").click());
    expect(luxSelectValue(container!, "选择字幕")).toBe("7");
    expect(container?.textContent).not.toContain("（当前）");

    for (let attempt = 0; attempt < 6 && speedButton(container!).textContent !== speedLabel(1.5); attempt += 1) {
      await act(async () => speedButton(container!).click());
    }
    expect(speedButton(container!).textContent).toBe(speedLabel(1.5));
    expect(firstVideo.playbackRate).toBe(1.5);

    firstVideo.currentTime = 48;
    firstVideo.volume = 0.4;
    firstVideo.muted = true;
    await act(async () => firstVideo.dispatchEvent(new Event("play")));

    await act(async () => trackMenuButton(container!, "音轨").click());
    await chooseLuxOptionByValue(container!, "选择音轨", "2");
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 20));
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(container?.querySelector('[data-testid="current-location"]')?.textContent)
      .toBe("/watch/movie-audio?subtitleStreamIndex=7&audioStreamIndex=2");
    expect(bootstrapSelections).toEqual([
      { audio: undefined, subtitle: 7 },
      { audio: 2, subtitle: 7 },
    ]);
    expect(lifecycle).toEqual([
      "create:source-audio:default",
      "stop:web-source-audio-default",
      "create:source-audio:2",
    ]);
    expect(api.webPlaybackEvent).toHaveBeenCalledWith(
      "web-source-audio-default",
      expect.objectContaining({
        state: "PLAYING",
        positionTicks: 480_000_000,
      }),
      false,
    );

    const replacementVideo = container?.querySelector<HTMLVideoElement>("video");
    if (!replacementVideo) throw new Error("replacement video was not rendered");
    primeVideo(replacementVideo);
    await act(async () => replacementVideo.dispatchEvent(new Event("loadedmetadata")));
    await act(async () => replacementVideo.dispatchEvent(new Event("canplay")));

    expect(replacementVideo.currentTime).toBe(48);
    expect(replacementVideo.playbackRate).toBe(1.5);
    expect(replacementVideo.volume).toBe(0.4);
    expect(replacementVideo.muted).toBe(true);
    expect(play).toHaveBeenCalled();
    expect(luxSelectValue(container!, "选择音轨")).toBe("2");
    expect(container?.textContent).toContain("（当前）");
    await act(async () => trackMenuButton(container!, "字幕").click());
    expect(luxSelectValue(container!, "选择字幕")).toBe("7");
  });

  it("keeps paused playback paused after switching audio", async () => {
    await renderPlayer("/watch/movie-audio?subtitleStreamIndex=7");

    const firstVideo = container?.querySelector<HTMLVideoElement>("video");
    if (!firstVideo) throw new Error("video element was not rendered");
    primeVideo(firstVideo);
    firstVideo.currentTime = 30;

    const settings = container?.querySelector<HTMLButtonElement>('[aria-label="音轨"]');
    if (!settings) throw new Error("settings control was not rendered");
    await act(async () => settings.click());

    await chooseLuxOptionByValue(container!, "选择音轨", "2");
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 20));
    });

    expect(api.webPlaybackEvent).toHaveBeenCalledWith(
      "web-source-audio-default",
      expect.objectContaining({ state: "PAUSED", positionTicks: 300_000_000 }),
      false,
    );
    const replacementVideo = container?.querySelector<HTMLVideoElement>("video");
    if (!replacementVideo) throw new Error("replacement video was not rendered");
    primeVideo(replacementVideo);
    await act(async () => replacementVideo.dispatchEvent(new Event("canplay")));

    expect(replacementVideo.currentTime).toBe(30);
    expect(play).not.toHaveBeenCalled();
  });

  it("keeps the existing video element mounted without an error layer while switching", async () => {
    let releaseBootstrap: (() => void) | undefined;
    const bootstrapGate = new Promise<void>((resolve) => {
      releaseBootstrap = resolve;
    });
    const originalBootstrap = vi.mocked(api.createWebPlaybackBootstrap).getMockImplementation();
    vi.mocked(api.createWebPlaybackBootstrap).mockImplementation(async (
      itemId,
      sourceId,
      capabilities,
      signal,
      selection,
    ) => {
      if (selection?.audioStreamIndex === 2) await bootstrapGate;
      if (!originalBootstrap) throw new Error("bootstrap mock missing");
      return originalBootstrap(itemId, sourceId, capabilities, signal, selection);
    });
    await renderPlayer("/watch/movie-audio?subtitleStreamIndex=7");

    const firstVideo = container?.querySelector<HTMLVideoElement>("video");
    if (!firstVideo) throw new Error("video element was not rendered");
    primeVideo(firstVideo);

    const settings = container?.querySelector<HTMLButtonElement>('[aria-label="音轨"]');
    if (!settings) throw new Error("settings control was not rendered");
    await act(async () => settings.click());

    await chooseLuxOptionByValue(container!, "选择音轨", "2");
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    expect(container?.querySelector("video")).toBe(firstVideo);
    expect(container?.querySelector('[role="alert"]')).toBeNull();
    expect(container?.textContent).toContain("正在切换音轨");

    releaseBootstrap?.();
    await settleReplacement("web-source-audio-2");
    expect(container?.querySelector("video")).not.toBeNull();
    expect(container?.querySelector('[role="alert"]')).toBeNull();
  });

  it("does not offer switching for a STRM source", async () => {
    vi.mocked(api.item).mockResolvedValue({
      ...item,
      mediaSources: [{
        ...item.mediaSources[0],
        sourceKind: "STRM_URL",
      }],
    });
    await renderPlayer("/watch/movie-audio");

    expect(container?.querySelector('[aria-label="音轨"]')).toBeNull();
    expect(container?.querySelector('[aria-label="选择音轨"]')).toBeNull();
  });

  it("rolls back instead of sticking when the replacement plan is unsupported", async () => {
    vi.mocked(api.createWebPlaybackSession).mockImplementation(async (
      _itemId,
      sourceId,
      _capabilities,
      _signal,
      selection,
    ) => {
      if (selection?.audioStreamIndex === 2) {
        return {
          sessionId: `web-${sourceId}-unsupported`,
          playSessionId: `lux-web:web-${sourceId}-unsupported`,
          sourceId,
          tier: 0,
          expiresAt: 1_900_000_000,
          plan: { type: "UNSUPPORTED", reason: "no audio tier" },
        };
      }
      return {
        sessionId: `web-${sourceId}`,
        playSessionId: `lux-web:web-${sourceId}`,
        sourceId,
        tier: 0,
        expiresAt: 1_900_000_000,
        plan: { type: "DIRECT", url: `/api/v1/playback/sessions/web-${sourceId}/direct?expires=1900000000&signature=test` },
      };
    });
    await renderPlayer("/watch/movie-audio");
    const settings = container?.querySelector<HTMLButtonElement>('[aria-label="音轨"]');
    if (!settings) throw new Error("settings control was not rendered");
    await act(async () => settings.click());

    await chooseLuxOptionByValue(container!, "选择音轨", "2");
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 30));
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(container?.querySelector('[data-testid="current-location"]')?.textContent)
      .toBe("/watch/movie-audio");
    expect(container?.textContent).toContain("音轨切换失败，已恢复原音轨。");
    const select = container?.querySelector<HTMLElement>('[aria-label="选择音轨"]');
    expect(select).not.toBeNull();
    expect(select?.getAttribute("disabled")).toBeNull();
  });

  it("cancels the pending switch when navigating to the next episode", async () => {
    const episodes = {
      "episode-audio-1": {
        id: "episode-audio-1",
        title: "第一集",
        itemType: "EPISODE" as const,
        seriesId: "series-audio",
        parentId: "season-audio",
        indexNumber: 1,
        mediaSources: [item.mediaSources[0]],
      },
      "episode-audio-2": {
        id: "episode-audio-2",
        title: "第二集",
        itemType: "EPISODE" as const,
        seriesId: "series-audio",
        parentId: "season-audio",
        indexNumber: 2,
        mediaSources: [{
          id: "source-next",
          isDefault: true,
          sourceKind: "LOCAL_FILE",
          durationTicks: 1_200_000_000,
          streams: [
            { index: 0, type: "VIDEO", codec: "hevc" },
            { index: 1, type: "AUDIO", codec: "aac" },
          ],
        }],
      },
    };
    vi.mocked(api.item).mockImplementation(async (itemId) => {
      return episodes[itemId as keyof typeof episodes] ?? episodes["episode-audio-1"];
    });
    vi.spyOn(api, "children").mockResolvedValue({ items: Object.values(episodes) });

    let releaseBootstrap: (() => void) | undefined;
    const bootstrapGate = new Promise<void>((resolve) => {
      releaseBootstrap = resolve;
    });
    const originalBootstrap = vi.mocked(api.createWebPlaybackBootstrap).getMockImplementation();
    vi.mocked(api.createWebPlaybackBootstrap).mockImplementation(async (
      itemId,
      sourceId,
      capabilities,
      signal,
      selection,
    ) => {
      if (itemId === "episode-audio-1" && !selection?.audioStreamIndex) await bootstrapGate;
      if (!originalBootstrap) throw new Error("bootstrap mock missing");
      return originalBootstrap(itemId, sourceId, capabilities, signal, selection);
    });

    await renderPlayer("/watch/episode-audio-1?audioStreamIndex=2");

    const firstVideo = container?.querySelector<HTMLVideoElement>("video");
    if (!firstVideo) throw new Error("video element was not rendered");
    primeVideo(firstVideo);
    firstVideo.currentTime = 48;
    firstVideo.volume = 0.4;
    firstVideo.muted = true;
    await act(async () => firstVideo.dispatchEvent(new Event("play")));

    const settings = container?.querySelector<HTMLButtonElement>('[aria-label="音轨"]');
    if (!settings) throw new Error("settings control was not rendered");
    await act(async () => settings.click());
    expect(luxSelectValue(container!, "选择音轨")).toBe("2");

    await chooseLuxOptionByValue(container!, "选择音轨", "1");
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    const next = container?.querySelector<HTMLButtonElement>('[aria-label="下一集"]');
    if (!next) throw new Error("next episode control was not rendered");
    await act(async () => next.click());
    releaseBootstrap?.();
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 20));
    });

    expect(container?.querySelector('[data-testid="current-location"]')?.textContent)
      .toBe("/watch/episode-audio-2");
    const nextVideo = await settleReplacement("source-next");
    expect(nextVideo.currentTime).toBe(0);
    expect(nextVideo.volume).toBe(1);
    expect(nextVideo.muted).toBe(false);
    expect(play).not.toHaveBeenCalled();
  });

  it("returns to the canonical URL when a server session switches back to its first audio track", async () => {
    await renderPlayer("/watch/movie-audio?subtitleStreamIndex=7");

    const settings = container?.querySelector<HTMLButtonElement>('[aria-label="音轨"]');
    if (!settings) throw new Error("settings control was not rendered");
    await act(async () => settings.click());

    await chooseLuxOptionByValue(container!, "选择音轨", "2");
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 20));
    });
    await settleReplacement("web-source-audio-2");
    expect(container?.querySelector('[data-testid="current-location"]')?.textContent)
      .toContain("audioStreamIndex=2");

    await chooseLuxOptionByValue(container!, "选择音轨", "1");
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 20));
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    // 服务端会话已按 0:a:0 复用第一条音轨，切回它时参数被删掉而不是写成 1。
    expect(container?.querySelector('[data-testid="current-location"]')?.textContent)
      .toBe("/watch/movie-audio?subtitleStreamIndex=7");
    expect(bootstrapSelections).toEqual([
      { audio: undefined, subtitle: 7 },
      { audio: 2, subtitle: 7 },
      { audio: undefined, subtitle: 7 },
    ]);
    expect(lifecycle).toEqual([
      "create:source-audio:default",
      "stop:web-source-audio-default",
      "create:source-audio:2",
      "stop:web-source-audio-2",
      "create:source-audio:default",
    ]);
    // 回到直连后不再有被确认的音轨。
    expect(container?.textContent).not.toContain("（当前）");
  });

  it("selects the first audio track explicitly from an unconfirmed Direct Play session", async () => {
    await renderPlayer("/watch/movie-audio?subtitleStreamIndex=7");

    const firstVideo = container?.querySelector<HTMLVideoElement>("video");
    if (!firstVideo) throw new Error("video element was not rendered");
    primeVideo(firstVideo);

    const settings = container?.querySelector<HTMLButtonElement>('[aria-label="音轨"]');
    if (!settings) throw new Error("settings control was not rendered");
    await act(async () => settings.click());

    // 直连且未显式选择：不声称任何音轨是当前音轨。
    expect(luxSelectValue(container!, "选择音轨")).toBe("1");
    expect(container?.textContent).not.toContain("（当前）");

    await chooseLuxOptionByValue(container!, "选择音轨", "1");
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 20));
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(container?.querySelector('[data-testid="current-location"]')?.textContent)
      .toBe("/watch/movie-audio?subtitleStreamIndex=7&audioStreamIndex=1");
    expect(bootstrapSelections).toEqual([
      { audio: undefined, subtitle: 7 },
      { audio: 1, subtitle: 7 },
    ]);
    expect(lifecycle).toEqual([
      "create:source-audio:default",
      "stop:web-source-audio-default",
      "create:source-audio:1",
    ]);
    // 显式选择后才有确认的当前音轨（服务端会话固定了它）。
    expect(container?.textContent).toContain("（当前）");
  });

  it("ignores a click on the first audio track a server session already maps", async () => {
    vi.mocked(api.createWebPlaybackSession).mockImplementation(async (_itemId, sourceId) => {
      lifecycle.push(`create:${sourceId}:server`);
      return {
        sessionId: "web-source-audio-server",
        playSessionId: "lux-web:web-source-audio-server",
        sourceId,
        tier: 2,
        expiresAt: 1_900_000_000,
        plan: {
          type: "SERVER_HLS" as const,
          manifestUrl: "/api/v1/playback/sessions/web-source-audio-server/manifest.m3u8?expires=1900000000&signature=test",
          tier: 2,
        },
      };
    });
    await renderPlayer("/watch/movie-audio?subtitleStreamIndex=7");

    const settings = container?.querySelector<HTMLButtonElement>('[aria-label="音轨"]');
    if (!settings) throw new Error("settings control was not rendered");
    await act(async () => settings.click());

    // 服务端会话固定 0:a:0，第一条音轨就是被确认的当前音轨。
    expect(luxSelectValue(container!, "选择音轨")).toBe("1");
    expect(container?.textContent).toContain("（当前）");

    await chooseLuxOptionByValue(container!, "选择音轨", "1");
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 20));
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    // URL 参数不变即视为重复选择：不重建会话、不进入切换中。
    expect(container?.querySelector('[data-testid="current-location"]')?.textContent)
      .toBe("/watch/movie-audio?subtitleStreamIndex=7");
    expect(lifecycle).toEqual(["create:source-audio:server"]);
    expect(container?.textContent).not.toContain("正在切换音轨");
  });

  it("hides the audio selector for a single-track source", async () => {
    vi.mocked(api.item).mockResolvedValue({
      ...item,
      mediaSources: [{
        ...item.mediaSources[0],
        streams: [
          { index: 0, type: "VIDEO", codec: "hevc" },
          { index: 1, type: "AUDIO", codec: "aac" },
        ],
      }],
    });
    await renderPlayer("/watch/movie-audio");

    expect(container?.querySelector('[aria-label="音轨"]')).toBeNull();
    expect(container?.querySelector('[aria-label="选择音轨"]')).toBeNull();
  });

  it("does not resurrect an abandoned target URL after the switch times out", async () => {
    let resolveReport: (() => void) | undefined;
    const reportGate = new Promise<{ accepted: boolean; duplicate: boolean; stale: boolean }>((resolve) => {
      resolveReport = () => resolve({ accepted: true, duplicate: false, stale: false });
    });
    vi.mocked(api.webPlaybackEvent).mockImplementation(() => reportGate);
    const timeoutSpy = vi.spyOn(window, "setTimeout");
    await renderPlayer("/watch/movie-audio");

    const settings = container?.querySelector<HTMLButtonElement>('[aria-label="音轨"]');
    if (!settings) throw new Error("settings control was not rendered");
    await act(async () => settings.click());

    await chooseLuxOptionByValue(container!, "选择音轨", "2");
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    // The URL must still be untouched while the position report is in flight.
    expect(container?.querySelector('[data-testid="current-location"]')?.textContent)
      .toBe("/watch/movie-audio");

    const timeout = timeoutSpy.mock.calls.find(([, delay]) => delay === 15_000);
    if (!timeout) throw new Error("switch timeout was not scheduled");
    await act(async () => {
      (timeout[0] as () => void)();
      await Promise.resolve();
    });

    expect(container?.querySelector('[data-testid="current-location"]')?.textContent)
      .toBe("/watch/movie-audio");
    expect(container?.textContent).toContain("音轨切换超时，已恢复原音轨。");

    resolveReport?.();
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    expect(container?.querySelector('[data-testid="current-location"]')?.textContent)
      .toBe("/watch/movie-audio");
    expect(container?.textContent).not.toContain("正在切换音轨");
  });

  it("reverts the URL and reports a notice when the new session fails", async () => {
    const originalBootstrap = vi.mocked(api.createWebPlaybackBootstrap).getMockImplementation();
    vi.mocked(api.createWebPlaybackBootstrap).mockImplementation(async (
      itemId,
      sourceId,
      capabilities,
      signal,
      selection,
    ) => {
      if (selection?.audioStreamIndex === 2) throw new Error("audio session failed");
      if (!originalBootstrap) throw new Error("bootstrap mock missing");
      return originalBootstrap(itemId, sourceId, capabilities, signal, selection);
    });
    vi.mocked(api.createWebPlaybackSession).mockImplementation(async (
      _itemId,
      sourceId,
      _capabilities,
      _signal,
      selection,
    ) => {
      if (selection?.audioStreamIndex === 2) throw new Error("audio session failed");
      return {
        sessionId: `web-${sourceId}`,
        playSessionId: `lux-web:web-${sourceId}`,
        sourceId,
        tier: 0,
        expiresAt: 1_900_000_000,
        plan: { type: "DIRECT", url: `/api/v1/playback/sessions/web-${sourceId}/direct?expires=1900000000&signature=test` },
      };
    });
    await renderPlayer("/watch/movie-audio");

    const settings = container?.querySelector<HTMLButtonElement>('[aria-label="音轨"]');
    if (!settings) throw new Error("settings control was not rendered");
    await act(async () => settings.click());

    await chooseLuxOptionByValue(container!, "选择音轨", "2");
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 30));
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(container?.querySelector('[data-testid="current-location"]')?.textContent)
      .toBe("/watch/movie-audio");
    expect(container?.textContent).toContain("音轨切换失败，已恢复原音轨。");
    expect(container?.querySelector('[aria-label="选择音轨"]')).not.toBeNull();
    expect(vi.mocked(api.createWebPlaybackBootstrap).mock.calls
      .filter((call) => call[4]?.audioStreamIndex === 2)).toHaveLength(1);
  });
});
