// @vitest-environment jsdom

import { act, useState } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { PlayerControls, type PlayerControlsProps } from "../src/features/player/components/player-controls";
import { PlayerOptionList } from "../src/features/player/components/player-option-list";
import { PlayerSettingsPanel, type PlayerSettingsTab } from "../src/features/player/components/player-settings-panel";
import { DEFAULT_DANMAKU_SETTINGS } from "../src/features/player/danmaku/settings";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("PlayerOptionList", () => {
  const options = [
    { value: "", label: "关闭字幕" },
    { value: "a", label: "简体中文", note: "内嵌 · ASS" },
    { value: "b", label: "繁体中文", disabled: true },
  ];

  it("exposes the group, marks the selected row and shows its note", () => {
    const markup = renderToStaticMarkup(<PlayerOptionList label="选择字幕" options={options} value="a" onSelect={() => undefined} />);
    expect(markup).toContain('role="radiogroup"');
    expect(markup).toContain('aria-label="选择字幕"');
    expect(markup).toContain('data-value="a"');
    expect(markup).toMatch(/aria-checked="true"[^>]*data-value="a"/);
    expect(markup).toContain("内嵌 · ASS");
    expect(markup.match(/aria-checked="true"/g)).toHaveLength(1);
  });

  it("selects the empty-value row and reports clicks, but not on disabled rows", async () => {
    const onSelect = vi.fn();
    const host = document.createElement("div");
    document.body.append(host);
    const root = createRoot(host);
    await act(async () => root.render(<PlayerOptionList label="选择字幕" options={options} value="" onSelect={onSelect} />));
    expect(host.querySelector('[role="radio"][data-value=""]')?.getAttribute("aria-checked")).toBe("true");
    await act(async () => host.querySelector<HTMLButtonElement>('[data-value="a"]')?.click());
    expect(onSelect).toHaveBeenCalledWith("a");
    expect(host.querySelector<HTMLButtonElement>('[data-value="b"]')?.disabled).toBe(true);
    await act(async () => root.unmount());
    host.remove();
  });

  it("disables every row when the group is disabled", () => {
    const markup = renderToStaticMarkup(<PlayerOptionList label="选择音轨" options={options} value="a" disabled onSelect={() => undefined} />);
    expect(markup.match(/disabled=""/g)).toHaveLength(3);
    expect(markup).toContain('aria-disabled="true"');
  });
});

describe("settings drawer tabs", () => {
  let root: Root | null = null;
  let host: HTMLDivElement | null = null;

  afterEach(async () => {
    if (root) await act(async () => root?.unmount());
    host?.remove();
    root = null;
    host = null;
  });

  function Harness({ withDanmaku = false }: { withDanmaku?: boolean }) {
    const [tab, setTab] = useState<PlayerSettingsTab>("quality");
    return (
      <PlayerSettingsPanel
        tab={tab}
        onTabChange={setTab}
        presentation={{
          loop: false,
          aspectRatio: "default",
          flip: "normal",
          onToggleLoop: () => undefined,
          onChangeAspectRatio: () => undefined,
          onChangeFlip: () => undefined,
        }}
        danmaku={withDanmaku ? { settings: DEFAULT_DANMAKU_SETTINGS, onChange: () => undefined, onReset: () => undefined } : undefined}
        info={{ groups: [{ title: "播放", rows: [{ label: "方式", value: "直连播放" }] }], copyText: "x" }}
        onClose={() => undefined}
      />
    );
  }

  async function mount(withDanmaku = false) {
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);
    await act(async () => root?.render(<Harness withDanmaku={withDanmaku} />));
    return host;
  }

  const tabs = (el: HTMLElement) => Array.from(el.querySelectorAll<HTMLButtonElement>('[role="tab"]'));

  it("switches the visible content with the tabs and marks the active one", async () => {
    const el = await mount();
    expect(tabs(el).map((tab) => tab.textContent?.trim())).toEqual(["画质", "长宽比", "播放信息"]);
    expect(el.querySelector('[role="switch"]')).not.toBeNull();
    expect(el.querySelector('[aria-label="长宽比"]')).toBeNull();

    await act(async () => tabs(el)[1]?.click());
    expect(tabs(el)[1]?.getAttribute("aria-selected")).toBe("true");
    expect(el.querySelector('[role="radiogroup"][aria-label="长宽比"]')).not.toBeNull();
    expect(el.querySelector('[role="switch"]')).toBeNull();

    await act(async () => tabs(el)[2]?.click());
    expect(el.textContent).toContain("直连播放");
  });

  it("adds a 弹幕 tab only when danmaku settings are provided", async () => {
    const el = await mount(true);
    expect(tabs(el).map((tab) => tab.textContent?.trim())).toEqual(["画质", "长宽比", "弹幕", "播放信息"]);
    await act(async () => tabs(el)[2]?.click());
    expect(el.querySelector('input[aria-label="弹幕不透明度"]')).not.toBeNull();
  });
});

describe("progress tooltip", () => {
  const noop = () => undefined;
  const props = (overrides: Partial<PlayerControlsProps>): PlayerControlsProps => ({
    playing: false, currentTime: 0, duration: 200, bufferedEnd: 0, volume: 1, muted: false, fullscreen: false,
    danmuVisible: true, settingsOpen: false, remainingTime: false, hoverTime: null, hoverPercent: null,
    progressBarRef: { current: null }, onTimelinePointerDown: noop, onTimelinePointerMove: noop, onTimelinePointerUp: noop,
    onTimelinePointerCancel: noop, onTimelineMouseMove: noop, onTimelineMouseLeave: noop, onTimelineKeyDown: noop,
    onTogglePlayPause: noop, onToggleMute: noop, onVolumeChange: noop, onToggleRemainingTime: noop, onToggleDanmu: noop,
    onToggleSettings: noop, onToggleFullscreen: noop, ...overrides,
  });
  const chapters = [
    { id: "c1", start: 0, end: 100, title: "开场", markerType: "CHAPTER", chapterIndex: 0 },
    { id: "c2", start: 100, end: 200, title: "第 2 章", markerType: "CHAPTER", chapterIndex: 1 },
  ] as const;

  it("shows the chapter name before the hovered time", () => {
    const markup = renderToStaticMarkup(<PlayerControls {...props({ hoverTime: 125, hoverPercent: 60, chapters })} />);
    expect(markup).toContain("第 2 章 · 02:05");
  });

  it("shows only the time when there are no chapters", () => {
    const markup = renderToStaticMarkup(<PlayerControls {...props({ hoverTime: 125, hoverPercent: 60 })} />);
    expect(markup).toContain(">02:05</div>");
    expect(markup).not.toContain(" · 02:05");
  });
});
