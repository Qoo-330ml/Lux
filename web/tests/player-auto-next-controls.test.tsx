// @vitest-environment jsdom

import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { PlayerAutoNextCard } from "../src/features/player/components/player-auto-next-card";
import { PlayerSettingsPanel } from "../src/features/player/components/player-settings-panel";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("LuxPlayer auto play next card", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;

  afterEach(() => {
    if (root) act(() => root?.unmount());
    root = undefined;
    container?.remove();
    container = undefined;
  });

  // One container and one root per test: a second call is a real rerender of the
  // same instance instead of a second root that would never be unmounted.
  function render(node: ReactNode) {
    if (!container) {
      container = document.createElement("div");
      document.body.append(container);
      root = createRoot(container);
    }
    return act(async () => root?.render(node));
  }

  function card() {
    return container?.querySelector<HTMLElement>("[aria-label='自动连播下一集']") ?? null;
  }

  function liveRegion() {
    return card()?.querySelector("[aria-live]") ?? null;
  }

  function countdownText() {
    return card()?.querySelector("[aria-hidden='true']")?.textContent ?? "";
  }

  it("announces the offer once and exposes labelled keyboard buttons", async () => {
    const onPlayNow = vi.fn();
    const onCancel = vi.fn();

    await render(
      <PlayerAutoNextCard secondsLeft={10} onPlayNow={onPlayNow} onCancel={onCancel} />,
    );

    expect(card()).not.toBeNull();
    // 单个 live 区域，不再叠加 role="status" 与 aria-live。
    expect(container?.querySelectorAll("[aria-live]")).toHaveLength(1);
    expect(card()?.querySelector("[role='status']")).toBeNull();
    expect(liveRegion()?.textContent).toContain("下一集");
    // 秒数不属于播报内容。
    expect(liveRegion()?.textContent).not.toContain("10 秒");
    expect(countdownText()).toContain("下一集将在 10 秒后播放");

    const playNow = container?.querySelector<HTMLButtonElement>("[aria-label='立即播放下一集']");
    const cancel = container?.querySelector<HTMLButtonElement>("[aria-label='取消自动连播']");
    expect(playNow?.textContent).toContain("立即播放");
    expect(cancel?.textContent).toContain("取消");

    await act(async () => playNow?.click());
    await act(async () => cancel?.click());
    expect(onPlayNow).toHaveBeenCalledTimes(1);
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  it("keeps the live region text stable while the countdown ticks on the same instance", async () => {
    const props = { onPlayNow: () => undefined, onCancel: () => undefined };

    await render(<PlayerAutoNextCard secondsLeft={10} {...props} />);
    const liveNode = liveRegion();
    const announcement = liveNode?.textContent ?? "";
    expect(announcement).toContain("下一集即将自动播放");
    expect(countdownText()).toContain("10 秒");

    await render(<PlayerAutoNextCard secondsLeft={3} {...props} />);

    // 同一实例 rerender：live 区域节点与文字都不变，辅助技术不会逐秒播报。
    expect(liveRegion()).toBe(liveNode);
    expect(liveRegion()?.textContent).toBe(announcement);
    expect(countdownText()).toContain("3 秒");
    // 没有遗留未卸载的旧 root。
    expect(document.body.querySelectorAll(".lux-player-auto-next")).toHaveLength(1);
  });
});

describe("LuxPlayer auto play next switch", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
  });

  async function renderPanel(enabled: boolean, onToggle: () => void) {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    await act(async () => root?.render(
      <PlayerSettingsPanel
        autoPlay={{ enabled, onToggle }}
        onClose={() => undefined}
      />,
    ));
  }

  it("renders a labelled switch that reflects the preference", async () => {
    const onToggle = vi.fn();
    await renderPanel(false, onToggle);

    const label = container?.querySelector("#lux-player-auto-play-label");
    expect(label?.textContent).toBe("自动连播下一集");
    const toggle = container?.querySelector<HTMLButtonElement>("[aria-label='自动连播下一集']");
    expect(toggle?.getAttribute("role")).toBe("switch");
    expect(toggle?.getAttribute("aria-checked")).toBe("false");
    expect(toggle?.textContent).toContain("已关闭");

    await act(async () => toggle?.click());
    expect(onToggle).toHaveBeenCalledTimes(1);
  });

  it("shows the enabled state", async () => {
    await renderPanel(true, () => undefined);
    const toggle = container?.querySelector("[aria-label='自动连播下一集']");
    expect(toggle?.getAttribute("aria-checked")).toBe("true");
    expect(toggle?.textContent).toContain("已开启");
  });
});
