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
    container?.remove();
  });

  function render(node: ReactNode) {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    return act(async () => root?.render(node));
  }

  it("announces the countdown and exposes labelled keyboard buttons", async () => {
    const onPlayNow = vi.fn();
    const onCancel = vi.fn();

    await render(
      <PlayerAutoNextCard secondsLeft={10} onPlayNow={onPlayNow} onCancel={onCancel} />,
    );

    expect(container?.querySelector("[aria-label='自动连播下一集']")).not.toBeNull();
    expect(container?.querySelector("[role='status']")?.textContent).toContain("10 秒");
    expect(container?.querySelector("[role='status']")?.textContent).toContain("下一集");

    const playNow = container?.querySelector<HTMLButtonElement>("[aria-label='立即播放下一集']");
    const cancel = container?.querySelector<HTMLButtonElement>("[aria-label='取消自动连播']");
    expect(playNow?.textContent).toContain("立即播放");
    expect(cancel?.textContent).toContain("取消");

    await act(async () => playNow?.click());
    await act(async () => cancel?.click());
    expect(onPlayNow).toHaveBeenCalledTimes(1);
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  it("keeps the countdown number in the status text while it ticks", async () => {
    const props = { onPlayNow: () => undefined, onCancel: () => undefined };

    await render(<PlayerAutoNextCard secondsLeft={10} {...props} />);
    expect(container?.querySelector("[role='status']")?.textContent).toContain("10 秒");

    await render(<PlayerAutoNextCard secondsLeft={3} {...props} />);
    expect(container?.querySelector("[role='status']")?.textContent).toContain("3 秒");
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
        playbackRates={[1]}
        playbackRate={1}
        onChangeRate={() => undefined}
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
