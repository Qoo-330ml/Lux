// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  PlayerPlaybackNoticeBanner,
  PlayerPlaybackNoticeLayer,
} from "../src/features/player/components/player-playback-notice";
import { PlayerVideoSurface } from "../src/features/player/components/player-video-surface";
import { playerFailure } from "../src/features/player/components/player-diagnostics";
import { playbackNoticeFor, type PlaybackNotice } from "../src/features/player/playback-notices";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

function notice(input: Parameters<typeof playbackNoticeFor>[0]): PlaybackNotice {
  const result = playbackNoticeFor(input);
  if (!result) throw new Error(`expected a notice for ${JSON.stringify(input)}`);
  return result;
}

function surfaceProps(overrides: Partial<Parameters<typeof PlayerVideoSurface>[0]> = {}) {
  return {
    streamUrl: "/signed/movie-1",
    title: "示例电影",
    videoRef: () => undefined,
    onClick: () => undefined,
    onDoubleClick: () => undefined,
    onError: () => undefined,
    centerSplash: null,
    fallbackLoading: false,
    fallbackSpeedX: null,
    errorMessage: null,
    showError: false,
    onRetry: () => undefined,
    onBack: () => undefined,
    ...overrides,
  } satisfies Parameters<typeof PlayerVideoSurface>[0];
}

describe("PlayerPlaybackNoticeLayer", () => {
  it("renders the reason, the action hint and the player actions as an alert", () => {
    const markup = renderToStaticMarkup(
      <PlayerPlaybackNoticeLayer
        notice={notice({ planType: "UNSUPPORTED", reason: "STRM_REQUIRES_DIRECT_PLAY" })}
        onRetry={() => undefined}
        onBack={() => undefined}
      />,
    );

    expect(markup).toContain('role="alert"');
    expect(markup).toContain("STRM 源只能直连播放");
    expect(markup).toContain(".strm 远程源在 Lux 中只支持直连播放");
    expect(markup).toContain("原生客户端");
    expect(markup).toContain('aria-label="重试"');
    expect(markup).toContain('aria-label="返回上一页"');
  });

  it("exposes the severity so warning states do not read like hard errors", () => {
    const markup = renderToStaticMarkup(
      <PlayerPlaybackNoticeLayer
        notice={notice({ status: 502 })}
        onRetry={() => undefined}
        onBack={() => undefined}
      />,
    );

    expect(markup).toContain("lux-player-notice-card-warning");
    expect(markup).toContain("服务端转码暂时不可用");
  });

  it("offers a retry that the player can wire to its own recovery", () => {
    const onRetry = vi.fn();
    const markup = renderToStaticMarkup(
      <PlayerPlaybackNoticeLayer
        notice={notice({ status: 403 })}
        onRetry={onRetry}
        onBack={() => undefined}
      />,
    );

    expect(markup).toContain("当前账号无权使用所需转码");
    expect(onRetry).not.toHaveBeenCalled();
  });
});

describe("PlayerPlaybackNoticeBanner", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;

  beforeEach(() => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
    root = undefined;
  });

  it("announces a transcoding session politely instead of blocking playback", () => {
    const markup = renderToStaticMarkup(
      <PlayerPlaybackNoticeBanner notice={notice({ planType: "SERVER_HLS", tier: 4 })} />,
    );

    expect(markup).toContain('role="status"');
    expect(markup).toContain('aria-live="polite"');
    expect(markup).toContain("服务端正在软件转码");
    expect(markup).toContain('aria-label="关闭提示"');
    expect(markup).not.toContain('role="alert"');
  });

  it("can be dismissed without touching the playback state", async () => {
    const onDismiss = vi.fn();
    const transcoding = notice({ planType: "SERVER_HLS", tier: 3 });

    await act(async () => {
      root?.render(<PlayerPlaybackNoticeBanner notice={transcoding} onDismiss={onDismiss} />);
    });
    expect(container?.querySelector(".lux-player-notice-banner")).not.toBeNull();

    const close = container?.querySelector<HTMLButtonElement>('[aria-label="关闭提示"]');
    await act(async () => {
      close?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });

    expect(container?.querySelector(".lux-player-notice-banner")).toBeNull();
    expect(onDismiss).toHaveBeenCalledTimes(1);
  });

  it("shows the banner again for the next playback session", async () => {
    const transcoding = notice({ planType: "SERVER_HLS", tier: 3 });

    await act(async () => {
      root?.render(<PlayerPlaybackNoticeBanner notice={transcoding} lifecycleKey="item-1:source:a" />);
    });
    await act(async () => {
      container?.querySelector<HTMLButtonElement>('[aria-label="关闭提示"]')
        ?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(container?.querySelector(".lux-player-notice-banner")).toBeNull();

    await act(async () => {
      root?.render(<PlayerPlaybackNoticeBanner notice={transcoding} lifecycleKey="item-1:source:b" />);
    });
    expect(container?.querySelector(".lux-player-notice-banner")).not.toBeNull();
  });

  it("renders nothing for a blocking notice", () => {
    const markup = renderToStaticMarkup(
      <PlayerPlaybackNoticeBanner notice={notice({ planType: "UNSUPPORTED", reason: "SERVER_TRANSCODING_DISABLED" })} />,
    );

    expect(markup).toBe("");
  });
});

describe("PlayerVideoSurface playback notices", () => {
  it("replaces the generic error layer with the UNSUPPORTED reason", () => {
    const markup = renderToStaticMarkup(
      <PlayerVideoSurface
        {...surfaceProps({
          streamUrl: "",
          showError: true,
          failure: playerFailure("BROWSER_UNSUPPORTED"),
          notice: notice({ planType: "UNSUPPORTED", reason: "BROWSER_CANNOT_CONSUME_HLS" }),
        })}
      />,
    );

    expect(markup).toContain("当前浏览器无法播放服务端 HLS");
    expect(markup).toContain("支持 MSE 的现代浏览器");
    expect(markup).not.toContain("浏览器不支持此媒体");
    expect(markup.match(/role="alert"/g)).toHaveLength(1);
  });

  it("keeps the generic error layer for engine failures without a notice", () => {
    const markup = renderToStaticMarkup(
      <PlayerVideoSurface
        {...surfaceProps({
          streamUrl: "",
          showError: true,
          failure: playerFailure("ENGINE_FAILED"),
        })}
      />,
    );

    expect(markup).toContain("播放器引擎失败");
    expect(markup).not.toContain("lux-player-notice-layer");
  });

  it("shows a transcode banner above the video without covering the controls", () => {
    const markup = renderToStaticMarkup(
      <PlayerVideoSurface
        {...surfaceProps({
          notice: notice({ planType: "SERVER_HLS", tier: 4, degraded: true }),
          noticeLifecycleKey: "movie-1:source:4",
        })}
      />,
    );

    expect(markup).toContain("lux-player-notice-banner");
    expect(markup).toContain("服务端正在软件转码");
    expect(markup).toContain('role="status"');
    expect(markup).not.toContain("lux-player-notice-layer");
  });

  it("hides the center play button while a blocking notice is shown", () => {
    const markup = renderToStaticMarkup(
      <PlayerVideoSurface
        {...surfaceProps({
          notice: notice({ planType: "UNSUPPORTED", reason: "NO_COMPATIBLE_SERVER_PLAN" }),
        })}
      />,
    );

    expect(markup).toContain("没有可用的服务端转码档位");
    expect(markup).not.toContain("lux-player-center-play");
  });

  it("does not stack a banner behind a blocking error layer", () => {
    const markup = renderToStaticMarkup(
      <PlayerVideoSurface
        {...surfaceProps({
          streamUrl: "",
          showError: true,
          failure: playerFailure("PLAYBACK_EXPIRED"),
          notice: notice({ planType: "SERVER_HLS", tier: 3 }),
        })}
      />,
    );

    expect(markup).toContain("播放地址已过期");
    expect(markup).not.toContain("lux-player-notice-banner");
  });
});
