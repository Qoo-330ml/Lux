// @vitest-environment jsdom

import { act } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { PlayerControls, type PlayerControlsProps } from "../src/features/player/components/player-controls";
import { PlayerSettingsPanel } from "../src/features/player/components/player-settings-panel";
import { PlayerSubtitleMenu } from "../src/features/player/components/player-subtitle-menu";
import { playerVideoPresentationSize, playerVideoPresentationStyle } from "../src/features/player/components/player-presentation";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

function controlProps(overrides: Partial<PlayerControlsProps> = {}): PlayerControlsProps {
  const noop = () => undefined;
  return {
    playing: false,
    currentTime: 30,
    duration: 120,
    bufferedEnd: 60,
    volume: 1,
    muted: false,
    fullscreen: false,
    danmuVisible: true,
    settingsOpen: false,
    remainingTime: false,
    hoverTime: null,
    hoverPercent: null,
    progressBarRef: { current: null },
    onTimelinePointerDown: noop,
    onTimelinePointerMove: noop,
    onTimelinePointerUp: noop,
    onTimelinePointerCancel: noop,
    onTimelineMouseMove: noop,
    onTimelineMouseLeave: noop,
    onTimelineKeyDown: noop,
    onTogglePlayPause: noop,
    onToggleMute: noop,
    onVolumeChange: noop,
    onToggleRemainingTime: noop,
    onToggleDanmu: noop,
    onToggleSettings: noop,
    onToggleFullscreen: noop,
    ...overrides,
  };
}

describe("control bar layout (static markup)", () => {
  it("shows current time left and total right above a full-width progress bar", () => {
    const markup = renderToStaticMarkup(<PlayerControls {...controlProps()} />);
    expect(markup).toMatch(/<div class="lux-player-timeline-times"><span class="lux-player-time-current">00:30<\/span><button type="button" class="lux-player-time-total"[^>]*>02:00<\/button><\/div>/);
    expect(markup.indexOf("lux-player-timeline-times")).toBeLessThan(markup.indexOf("lux-player-timeline-interaction"));
  });

  it("no longer renders screenshot, keyboard-help, info or PiP controls in the bar", () => {
    const markup = renderToStaticMarkup(<PlayerControls {...controlProps()} />);
    expect(markup).not.toContain('aria-label="截图"');
    expect(markup).not.toContain('aria-label="键盘快捷键"');
    expect(markup).not.toContain('aria-label="播放信息"');
    expect(markup).not.toContain('aria-label="画中画"');
  });

  it("renders subtitle on the left and audio, speed, danmaku, settings and fullscreen on the right", () => {
    const markup = renderToStaticMarkup(
      <PlayerControls
        {...controlProps({
          subtitleMenu: <div>字幕菜单</div>,
          audioMenu: <div>音轨菜单</div>,
          playbackRate: 1,
          playbackRates: [1, 1.5],
          onChangeRate: () => undefined,
        })}
      />,
    );
    const left = markup.slice(markup.indexOf('class="lux-player-controls-left"'), markup.indexOf('class="lux-player-controls-middle"'));
    const right = markup.slice(markup.indexOf('class="lux-player-controls-right"'));
    expect(left).toContain('aria-label="字幕"');
    expect(left).not.toContain('aria-label="音轨"');
    expect(right).toContain('aria-label="音轨"');
    expect(right).toContain('aria-label="倍速"');
    expect(right).toContain('aria-label="播放器设置"');
    expect(right).toContain('aria-label="全屏"');
    expect(right).not.toContain('aria-label="字幕"');
  });

  it("shows the seek buttons only when a seek handler is supplied", () => {
    expect(renderToStaticMarkup(<PlayerControls {...controlProps()} />)).not.toContain('aria-label="后退 10 秒"');
    const markup = renderToStaticMarkup(<PlayerControls {...controlProps({ onSeekBy: () => undefined })} />);
    expect(markup).toContain('aria-label="后退 10 秒"');
    expect(markup).toContain('aria-label="前进 10 秒"');
  });
});

describe("control bar popovers and speed (DOM)", () => {
  let container: HTMLDivElement | null = null;
  let root: Root | null = null;

  afterEach(async () => {
    if (root) await act(async () => root?.unmount());
    container?.remove();
    container = null;
    root = null;
    document.querySelectorAll(".lux-select-menu").forEach((node) => node.remove());
  });

  async function mount(props: Partial<PlayerControlsProps> = {}) {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    await act(async () => {
      root?.render(
        <PlayerControls
          {...controlProps({
            subtitleMenu: <div data-testid="subs">字幕菜单</div>,
            audioMenu: <div data-testid="audio">音轨菜单</div>,
            playbackRate: 1,
            playbackRates: [1, 1.5, 2],
            onChangeRate: vi.fn(),
            ...props,
          })}
        />,
      );
    });
    if (!container) throw new Error("container missing");
    return container;
  }

  const button = (host: HTMLElement, label: string) => {
    const el = host.querySelector<HTMLButtonElement>(`button[aria-label="${label}"]`);
    if (!el) throw new Error(`${label} button missing`);
    return el;
  };

  it("opens the subtitle popover from its button and reports the state", async () => {
    const host = await mount();
    expect(host.querySelector('[data-testid="subs"]')).toBeNull();
    await act(async () => button(host, "字幕").click());
    expect(host.querySelector('[data-testid="subs"]')).not.toBeNull();
    expect(button(host, "字幕").getAttribute("aria-expanded")).toBe("true");
  });

  it("switches between subtitle and audio popovers (one open at a time)", async () => {
    const host = await mount();
    await act(async () => button(host, "字幕").click());
    await act(async () => button(host, "音轨").click());
    expect(host.querySelector('[data-testid="audio"]')).not.toBeNull();
    expect(host.querySelector('[data-testid="subs"]')).toBeNull();
  });

  it("closes an open popover on a pointer press outside the control bar", async () => {
    const host = await mount();
    await act(async () => button(host, "字幕").click());
    await act(async () => {
      document.body.dispatchEvent(new MouseEvent("pointerdown", { bubbles: true }));
    });
    expect(host.querySelector('[data-testid="subs"]')).toBeNull();
  });

  it("keeps the popover open for presses inside it and for LuxSelect option lists rendered in the body", async () => {
    const host = await mount();
    await act(async () => button(host, "音轨").click());
    await act(async () => {
      host.querySelector('[data-testid="audio"]')?.dispatchEvent(new MouseEvent("pointerdown", { bubbles: true }));
    });
    expect(host.querySelector('[data-testid="audio"]')).not.toBeNull();

    const menu = document.createElement("div");
    menu.className = "lux-select-menu";
    document.body.append(menu);
    await act(async () => {
      menu.dispatchEvent(new MouseEvent("pointerdown", { bubbles: true }));
    });
    expect(host.querySelector('[data-testid="audio"]')).not.toBeNull();
  });

  it("closes an open popover on Escape", async () => {
    const host = await mount();
    await act(async () => button(host, "字幕").click());
    await act(async () => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    });
    expect(host.querySelector('[data-testid="subs"]')).toBeNull();
  });

  it("cycles the speed control through the supplied rates", async () => {
    const onChangeRate = vi.fn();
    const host = await mount({ onChangeRate, playbackRate: 1, playbackRates: [1, 1.5, 2] });
    const speed = host.querySelector<HTMLButtonElement>(".lux-player-speed-btn");
    expect(speed?.textContent).toBe("标准");
    await act(async () => speed?.click());
    expect(onChangeRate).toHaveBeenLastCalledWith(1.5);
  });

  it("calls the seek handler with -10 and +10 seconds", async () => {
    const onSeekBy = vi.fn();
    const host = await mount({ onSeekBy });
    await act(async () => button(host, "后退 10 秒").click());
    await act(async () => button(host, "前进 10 秒").click());
    expect(onSeekBy).toHaveBeenNthCalledWith(1, -10);
    expect(onSeekBy).toHaveBeenNthCalledWith(2, 10);
  });
});

describe("settings panel and subtitle menu", () => {
  it("offers the playback information entry instead of subtitles, audio or speed", () => {
    const onOpenInfo = vi.fn();
    const markup = renderToStaticMarkup(<PlayerSettingsPanel onOpenInfo={() => undefined} onClose={() => undefined} />);
    expect(markup).toContain("播放信息");
    expect(markup).not.toContain('aria-label="选择字幕"');
    expect(markup).not.toContain("播放速度");
    expect(onOpenInfo).not.toHaveBeenCalled();
  });

  it("keeps the subtitle offset with the subtitle choice", () => {
    const markup = renderToStaticMarkup(<PlayerSubtitleMenu captions={[]} captionOffset={0} />);
    expect(markup).toContain('aria-label="选择字幕"');
    expect(markup).toContain('id="lux-player-caption-offset"');
  });
});

describe("aspect ratio fill", () => {
  it("fills the stage by cropping rather than letterboxing", () => {
    expect(playerVideoPresentationSize("fill", 1600, 900)).toBeNull();
    expect(playerVideoPresentationStyle("fill", "normal", null)).toEqual({ objectFit: "cover" });
    expect(playerVideoPresentationStyle("fill", "horizontal", null)).toEqual({ objectFit: "cover", transform: "scaleX(-1)" });
  });
});
