import { vi } from "vitest";
import { api } from "../src/lib/api/client";

export function mockPlaybackBootstrap() {
  return vi.spyOn(api, "createWebPlaybackBootstrap").mockImplementation(async (
    itemId,
    requestedSourceId,
    capabilities,
  ) => {
    const [item, playback] = await Promise.all([
      api.item(itemId),
      api.playback(itemId).catch(() => ({})),
    ]);
    const source = requestedSourceId
      ? item.mediaSources?.find((entry) => entry.id === requestedSourceId)
      : item.mediaSources?.find((entry) => entry.isDefault) ?? item.mediaSources?.[0];
    if (!source) throw new Error("test item has no playback source");
    const session = await api.createWebPlaybackSession(itemId, source.id, capabilities);
    return { item, playback, session };
  });
}

/**
 * Subtitles and audio tracks live in control-bar popovers now (not the settings
 * panel). Returns the button that opens the named popover.
 */
export function trackMenuButton(container: HTMLElement, name: "字幕" | "音轨"): HTMLButtonElement {
  const button = container.querySelector<HTMLButtonElement>(`button[aria-label="${name}"]`);
  if (!button) throw new Error(`${name} control was not rendered in the control bar`);
  return button;
}

/** Speed is a cycling control in the control bar; this clicks it until it reads the target rate. */
export function speedButton(container: HTMLElement): HTMLButtonElement {
  const button = container.querySelector<HTMLButtonElement>(".lux-player-speed-btn");
  if (!button) throw new Error("speed control was not rendered in the control bar");
  return button;
}

/** The speed button is icon-only; its tooltip carries the current rate. */
export function speedLabel(rate: number): string {
  return `倍速：${rate === 1 ? "标准" : `${rate}x`}`;
}
