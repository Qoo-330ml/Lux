// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { PlayerPage } from "../src/features/player/PlayerPage";
import { ApiError, api } from "../src/lib/api/client";
import { mockPlaybackBootstrap } from "./player-test-helpers";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const movie = {
  id: "movie-notice",
  title: "提示示例",
  itemType: "MOVIE",
  mediaSources: [{
    id: "source-notice",
    isDefault: true,
    sourceKind: "LOCAL_FILE",
    container: "mp4",
    streams: [{ index: 0, type: "VIDEO", codec: "H264" }],
  }],
};

describe("PlayerPage unified playback notices", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;

  beforeEach(() => {
    window.localStorage.clear();
    mockPlaybackBootstrap();
    vi.spyOn(api, "item").mockResolvedValue(movie);
    vi.spyOn(api, "playback").mockResolvedValue({
      positionTicks: 0,
      isPlayed: false,
      state: null,
      isPaused: false,
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
    container = undefined;
    root = undefined;
    vi.restoreAllMocks();
  });

  async function renderPlayer() {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/watch/movie-notice"]}>
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
    return container;
  }

  it("shows the UNSUPPORTED reason instead of the generic unsupported copy", async () => {
    vi.spyOn(api, "createWebPlaybackSession").mockResolvedValue({
      sessionId: null,
      playSessionId: null,
      sourceId: "source-notice",
      tier: 0,
      expiresAt: 1_900_000_000,
      plan: { type: "UNSUPPORTED", reason: "STRM_REQUIRES_DIRECT_PLAY" },
    });

    const rendered = await renderPlayer();

    expect(rendered.querySelector(".lux-player-notice-layer")?.textContent)
      .toContain("STRM 源只能直连播放");
    expect(rendered.textContent).toContain("原生客户端");
    expect(rendered.textContent).not.toContain("浏览器不支持此媒体");
    expect(rendered.textContent).not.toContain("服务端播放计划失败");
    // The blocking layer owns its own actions instead of the generic error card.
    expect(rendered.querySelector(".lux-player-error-card")).toBeNull();
    expect(rendered.querySelector('[aria-label="重试"]')).not.toBeNull();
  });

  it("explains a 502 session rejection as exhausted transcode capacity", async () => {
    const failure = new ApiError("服务端 HLS 暂时不可用", { status: 502, code: "INTERNAL" });
    vi.spyOn(api, "createWebPlaybackSession").mockRejectedValue(failure);

    const rendered = await renderPlayer();

    expect(rendered.querySelector('[role="alert"]')?.textContent)
      .toContain("服务端转码暂时不可用");
    expect(rendered.textContent).toContain("转码并发");
    expect(rendered.textContent).toContain("稍后重试");
    expect(rendered.querySelector('[aria-label="重试"]')).not.toBeNull();
  });

  it("asks the viewer to refresh when the CSRF check fails", async () => {
    vi.spyOn(api, "createWebPlaybackSession").mockRejectedValue(
      new ApiError("CSRF 校验失败", { status: 403, code: "CSRF_FAILED" }),
    );

    const rendered = await renderPlayer();

    expect(rendered.querySelector('[role="alert"]')?.textContent).toContain("请求状态已失效");
    expect(rendered.textContent).toContain("重新登录");
    expect(rendered.textContent).not.toContain("播放策略");
  });

  it("keeps the generic plan failure copy for unknown rejections", async () => {
    vi.spyOn(api, "createWebPlaybackSession").mockRejectedValue(new Error("network down"));

    const rendered = await renderPlayer();

    expect(rendered.textContent).toContain("服务端播放计划失败");
    expect(rendered.textContent).not.toContain("转码并发");
  });

  it("does not keep a stale bootstrap failure over a recovered plan", async () => {
    vi.spyOn(api, "createWebPlaybackBootstrap").mockRejectedValue(
      new ApiError("服务端 HLS 暂时不可用", { status: 502, code: "INTERNAL" }),
    );
    vi.spyOn(api, "createWebPlaybackSession").mockResolvedValue({
      sessionId: "web-recovered",
      playSessionId: "lux-web:web-recovered",
      sourceId: "source-notice",
      tier: 0,
      expiresAt: 1_900_000_000,
      plan: {
        type: "DIRECT",
        url: "/api/v1/playback/sessions/web-recovered/direct?expires=1900000000&signature=test",
      },
    });

    const rendered = await renderPlayer();

    expect(rendered.querySelector(".lux-player-notice-layer")).toBeNull();
    expect(rendered.textContent).not.toContain("服务端转码暂时不可用");
    expect(rendered.querySelector<HTMLVideoElement>("video")?.getAttribute("src"))
      .toContain("/api/v1/playback/sessions/web-recovered/direct");
  });
});
