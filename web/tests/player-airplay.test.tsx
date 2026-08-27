// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { usePlayerAirPlay } from "../src/features/player/components/player-airplay";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

function AirPlayHarness({ video, lifecycleKey }: { video: HTMLVideoElement | null; lifecycleKey: string }) {
  const { available, showPicker } = usePlayerAirPlay(video, lifecycleKey);
  return available ? <button type="button" aria-label="AirPlay" onClick={showPicker}>AirPlay</button> : null;
}

describe("LuxPlayer AirPlay capability gate", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;
  let availabilityDescriptor: PropertyDescriptor | undefined;

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
    if (availabilityDescriptor) {
      Object.defineProperty(window, "WebKitPlaybackTargetAvailabilityEvent", availabilityDescriptor);
    } else {
      delete (window as Window & { WebKitPlaybackTargetAvailabilityEvent?: unknown }).WebKitPlaybackTargetAvailabilityEvent;
    }
  });

  it("waits for an available target, calls the current video picker, and hides when unavailable", async () => {
    availabilityDescriptor = Object.getOwnPropertyDescriptor(window, "WebKitPlaybackTargetAvailabilityEvent");
    Object.defineProperty(window, "WebKitPlaybackTargetAvailabilityEvent", {
      configurable: true,
      value: function WebKitPlaybackTargetAvailabilityEvent() { return undefined; },
    });
    const video = document.createElement("video");
    const showPicker = vi.fn();
    Object.defineProperty(video, "webkitShowPlaybackTargetPicker", { configurable: true, value: showPicker });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    await act(async () => {
      root?.render(<AirPlayHarness video={video} lifecycleKey="source-1" />);
    });
    expect(container.querySelector("[aria-label='AirPlay']")).toBeNull();

    await act(async () => {
      video.dispatchEvent(Object.assign(new Event("webkitplaybacktargetavailabilitychanged"), { availability: "available" }));
    });
    const button = container.querySelector<HTMLButtonElement>("[aria-label='AirPlay']");
    expect(button).not.toBeNull();
    await act(async () => button?.click());
    expect(showPicker).toHaveBeenCalledTimes(1);

    await act(async () => {
      video.dispatchEvent(Object.assign(new Event("webkitplaybacktargetavailabilitychanged"), { availability: "not-available" }));
    });
    expect(container.querySelector("[aria-label='AirPlay']")).toBeNull();
  });

  it("removes the old availability listener when the playback lifecycle changes", async () => {
    availabilityDescriptor = Object.getOwnPropertyDescriptor(window, "WebKitPlaybackTargetAvailabilityEvent");
    Object.defineProperty(window, "WebKitPlaybackTargetAvailabilityEvent", {
      configurable: true,
      value: function WebKitPlaybackTargetAvailabilityEvent() { return undefined; },
    });
    const firstVideo = document.createElement("video");
    const secondVideo = document.createElement("video");
    for (const video of [firstVideo, secondVideo]) {
      Object.defineProperty(video, "webkitShowPlaybackTargetPicker", { configurable: true, value: vi.fn() });
    }
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    await act(async () => root?.render(<AirPlayHarness video={firstVideo} lifecycleKey="source-1" />));
    await act(async () => root?.render(<AirPlayHarness video={secondVideo} lifecycleKey="source-2" />));
    await act(async () => {
      firstVideo.dispatchEvent(Object.assign(new Event("webkitplaybacktargetavailabilitychanged"), { availability: "available" }));
    });
    expect(container.querySelector("[aria-label='AirPlay']")).toBeNull();
    await act(async () => {
      secondVideo.dispatchEvent(Object.assign(new Event("webkitplaybacktargetavailabilitychanged"), { availability: "available" }));
    });
    expect(container.querySelector("[aria-label='AirPlay']")).not.toBeNull();
  });
});
