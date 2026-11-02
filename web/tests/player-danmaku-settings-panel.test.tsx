// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PlayerDanmakuSettings } from "../src/features/player/components/player-danmaku-settings";
import { PlayerSettingsPanel } from "../src/features/player/components/player-settings-panel";
import { useDanmakuSettings } from "../src/features/player/danmaku/useDanmakuSettings";
import {
  DANMAKU_SETTINGS_STORAGE_KEY,
  DEFAULT_DANMAKU_SETTINGS,
  readDanmakuSettings,
  type DanmakuSettings,
} from "../src/features/player/danmaku/settings";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

function byAriaLabel<T extends HTMLElement>(container: HTMLElement, label: string, selector = "*"): T | null {
  return [...container.querySelectorAll<T>(selector)]
    .find((element) => element.getAttribute("aria-label") === label) ?? null;
}

function buttonByText(container: HTMLElement, text: string): HTMLButtonElement | null {
  return [...container.querySelectorAll<HTMLButtonElement>("button")]
    .find((button) => button.textContent?.trim() === text) ?? null;
}

/** React's controlled inputs read the native value setter, so jsdom needs it explicitly. */
async function changeInput(input: HTMLInputElement, value: string) {
  await act(async () => {
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
    setter?.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

async function click(element: HTMLElement) {
  await act(async () => {
    element.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

describe("LuxPlayer danmaku settings panel", () => {
  let container: HTMLDivElement;
  let root: Root;

  const renderSettings = async (
    settings: DanmakuSettings = DEFAULT_DANMAKU_SETTINGS,
    onChange = vi.fn(),
    onReset = vi.fn(),
  ) => {
    await act(async () => {
      root.render(<PlayerDanmakuSettings settings={settings} onChange={onChange} onReset={onReset} />);
    });
    return { onChange, onReset };
  };

  beforeEach(() => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("renders labelled controls for every documented setting", async () => {
    await renderSettings();

    expect(container.querySelector('[data-settings-section="danmaku"]')).not.toBeNull();
    const opacity = byAriaLabel<HTMLInputElement>(container, "弹幕不透明度", "input")!;
    expect(opacity.getAttribute("type")).toBe("range");
    // Whole percentages, not a floating-point leak from 0.1 * 100.
    expect(opacity.getAttribute("min")).toBe("10");
    expect(opacity.getAttribute("max")).toBe("100");
    expect(opacity.getAttribute("step")).toBe("5");
    expect(byAriaLabel(container, "弹幕字号", "input")).not.toBeNull();
    expect(byAriaLabel(container, "弹幕速度", "input")).not.toBeNull();
    expect(byAriaLabel(container, "弹幕时间偏移", "input")).not.toBeNull();
    expect(byAriaLabel(container, "弹幕显示区域", "div")?.getAttribute("role")).toBe("group");
    expect(byAriaLabel(container, "屏蔽滚动弹幕", "button")).not.toBeNull();
    expect(buttonByText(container, "恢复默认")).not.toBeNull();
    expect(container.textContent).toContain("弹幕");
  });

  it("reports opacity as a percentage and offsets with a sign", async () => {
    const { onChange } = await renderSettings({ ...DEFAULT_DANMAKU_SETTINGS, opacity: 0.4, offsetSeconds: 2.5 });

    const opacity = byAriaLabel<HTMLInputElement>(container, "弹幕不透明度", "input")!;
    expect(opacity.value).toBe("40");
    expect(opacity.getAttribute("aria-valuetext")).toBe("40%");

    const offset = byAriaLabel<HTMLInputElement>(container, "弹幕时间偏移", "input")!;
    expect(offset.value).toBe("2.5");
    expect(offset.getAttribute("aria-valuetext")).toBe("+2.5s");

    await changeInput(opacity, "55");
    expect(onChange).toHaveBeenCalledWith({ opacity: 0.55 });

    await changeInput(offset, "-1.5");
    expect(onChange).toHaveBeenCalledWith({ offsetSeconds: -1.5 });
  });

  it("changes the text scale and the speed multiplier", async () => {
    const { onChange } = await renderSettings();

    await changeInput(byAriaLabel<HTMLInputElement>(container, "弹幕字号", "input")!, "1.5");
    expect(onChange).toHaveBeenCalledWith({ fontScale: 1.5 });

    await changeInput(byAriaLabel<HTMLInputElement>(container, "弹幕速度", "input")!, "0.5");
    expect(onChange).toHaveBeenCalledWith({ speed: 0.5 });
  });

  it("switches the display area and the blocked danmaku types", async () => {
    const { onChange } = await renderSettings({ ...DEFAULT_DANMAKU_SETTINGS, blockedModes: ["top"] });

    const half = buttonByText(container, "上 1/2")!;
    expect(half.getAttribute("aria-pressed")).toBe("false");
    await click(half);
    expect(onChange).toHaveBeenCalledWith({ area: "half" });

    const quarter = byAriaLabel<HTMLButtonElement>(container, "上 1/4", "button")!;
    await click(quarter);
    expect(onChange).toHaveBeenCalledWith({ area: "quarter" });

    const topSwitch = byAriaLabel<HTMLButtonElement>(container, "屏蔽顶部弹幕", "button")!;
    expect(topSwitch.getAttribute("role")).toBe("switch");
    expect(topSwitch.getAttribute("aria-checked")).toBe("true");
    await click(topSwitch);
    expect(onChange).toHaveBeenCalledWith({ blockedModes: [] });

    await click(byAriaLabel<HTMLButtonElement>(container, "屏蔽底部弹幕", "button")!);
    expect(onChange).toHaveBeenCalledWith({ blockedModes: ["top", "bottom"] });
  });

  it("adds and removes keywords with plain-text validation", async () => {
    const { onChange } = await renderSettings({ ...DEFAULT_DANMAKU_SETTINGS, keywords: ["剧透"] });

    const input = byAriaLabel<HTMLInputElement>(container, "屏蔽关键词", "input")!;
    const add = byAriaLabel<HTMLButtonElement>(container, "添加屏蔽关键词", "button")!;
    expect(add.disabled).toBe(true);

    await changeInput(input, "  前排  ");
    await click(add);
    expect(onChange).toHaveBeenCalledWith({ keywords: ["剧透", "前排"] });

    // Duplicates (case-insensitively) and over-long keywords are rejected instead of stored.
    await changeInput(input, "剧透");
    expect(byAriaLabel<HTMLButtonElement>(container, "添加屏蔽关键词", "button")!.disabled).toBe(true);
    await changeInput(input, "x".repeat(31));
    expect(byAriaLabel<HTMLButtonElement>(container, "添加屏蔽关键词", "button")!.disabled).toBe(true);

    await click(byAriaLabel<HTMLButtonElement>(container, "删除屏蔽词 剧透", "button")!);
    expect(onChange).toHaveBeenCalledWith({ keywords: [] });
  });

  it("disables adding keywords at the documented limit", async () => {
    const keywords = Array.from({ length: 50 }, (_, index) => `词${index}`);
    await renderSettings({ ...DEFAULT_DANMAKU_SETTINGS, keywords });

    const input = byAriaLabel<HTMLInputElement>(container, "屏蔽关键词", "input")!;
    await changeInput(input, "新词");
    expect(byAriaLabel<HTMLButtonElement>(container, "添加屏蔽关键词", "button")!.disabled).toBe(true);
    expect(container.textContent).toContain("50/50");
  });

  it("resets every setting from the panel action", async () => {
    const { onReset } = await renderSettings({ ...DEFAULT_DANMAKU_SETTINGS, opacity: 0.3 });
    await click(byAriaLabel<HTMLButtonElement>(container, "恢复默认弹幕设置", "button")!);
    expect(onReset).toHaveBeenCalledTimes(1);
  });

  it("is part of the existing player settings dialog", async () => {
    const onClose = vi.fn();
    await act(async () => {
      root.render(
        <PlayerSettingsPanel
          danmaku={{
            settings: DEFAULT_DANMAKU_SETTINGS,
            onChange: vi.fn(),
            onReset: vi.fn(),
          }}
          onClose={onClose}
        />,
      );
    });

    const dialog = byAriaLabel<HTMLElement>(container, "播放设置", "div")!;
    expect(dialog.getAttribute("role")).toBe("dialog");
    expect(byAriaLabel(container, "弹幕不透明度", "input")).not.toBeNull();
  });

  it("keeps the danmaku section out when the page does not pass it", async () => {
    await act(async () => {
      root.render(<PlayerSettingsPanel onClose={vi.fn()} />);
    });
    expect(container.querySelector('[data-settings-section="danmaku"]')).toBeNull();
  });
});

describe("LuxPlayer danmaku settings persistence", () => {
  let container: HTMLDivElement;
  let root: Root;

  function Harness() {
    const { settings, updateSettings, resetSettings } = useDanmakuSettings();
    return (
      <PlayerDanmakuSettings
        settings={settings}
        onChange={updateSettings}
        onReset={resetSettings}
      />
    );
  }

  const mount = async () => {
    await act(async () => {
      root.render(<Harness />);
    });
  };

  beforeEach(() => {
    window.localStorage.clear();
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    window.localStorage.clear();
  });

  it("persists a change and restores it on the next mount", async () => {
    await mount();
    await click(byAriaLabel<HTMLButtonElement>(container, "上 1/4", "button")!);

    expect(readDanmakuSettings().area).toBe("quarter");
    expect(JSON.parse(window.localStorage.getItem(DANMAKU_SETTINGS_STORAGE_KEY)!)).toMatchObject({
      area: "quarter",
    });

    act(() => root.unmount());
    root = createRoot(container);
    await mount();
    expect(byAriaLabel<HTMLButtonElement>(container, "上 1/4", "button")!.getAttribute("aria-pressed"))
      .toBe("true");
  });

  it("writes the defaults back when the viewer resets", async () => {
    window.localStorage.setItem(
      DANMAKU_SETTINGS_STORAGE_KEY,
      JSON.stringify({ ...DEFAULT_DANMAKU_SETTINGS, opacity: 0.2, keywords: ["剧透"] }),
    );
    await mount();
    await click(byAriaLabel<HTMLButtonElement>(container, "恢复默认弹幕设置", "button")!);

    expect(readDanmakuSettings()).toEqual(DEFAULT_DANMAKU_SETTINGS);
  });

  it("ignores corrupted storage without throwing in the panel", async () => {
    window.localStorage.setItem(DANMAKU_SETTINGS_STORAGE_KEY, "{oops");
    await mount();
    expect(byAriaLabel<HTMLInputElement>(container, "弹幕不透明度", "input")!.value).toBe("100");
  });
});
