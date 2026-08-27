// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { resolvePlayerGesture } from "../src/features/player/components/player-gestures";
import { PlayerVideoSurface } from "../src/features/player/components/player-video-surface";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let root: Root | undefined;
let container: HTMLDivElement | undefined;

afterEach(() => {
  vi.useRealTimers();
  if (root) act(() => root?.unmount());
  root = undefined;
  container?.remove();
  container = undefined;
});

function dispatchPointer(
  target: Element,
  type: "pointerdown" | "pointermove" | "pointerup" | "pointercancel",
  options: { pointerId: number; pointerType: string; clientX: number; clientY: number },
) {
  const event = new Event(type, { bubbles: true, cancelable: true });
  Object.defineProperties(event, {
    pointerId: { value: options.pointerId },
    pointerType: { value: options.pointerType },
    clientX: { value: options.clientX },
    clientY: { value: options.clientY },
  });
  act(() => target.dispatchEvent(event));
}

function renderGestureSurface() {
  const onClick = vi.fn();
  const onDoubleClick = vi.fn();
  const onSeekTo = vi.fn();
  const onVolumeChange = vi.fn();
  const onSeekRelative = vi.fn();
  const onSingleTap = vi.fn();
  const onActivity = vi.fn();
  const onInteractionChange = vi.fn();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);

  act(() => {
    root?.render(createElement(PlayerVideoSurface, {
      streamUrl: "/signed/movie",
      title: "示例电影",
      videoRef: () => undefined,
      onClick,
      onDoubleClick,
      centerSplash: null,
      fallbackLoading: false,
      fallbackSpeedX: null,
      errorMessage: null,
      showError: false,
      onRetry: () => undefined,
      onBack: () => undefined,
      gestureOptions: {
          currentTime: 20,
          duration: 160,
          volume: 0.5,
          onSeekTo,
          onVolumeChange,
          onSeekRelative,
          onSingleTap,
          onActivity,
          onInteractionChange,
      },
    }));
  });

  const video = container.querySelector<HTMLVideoElement>("video");
  if (!video) throw new Error("video element was not rendered");
  vi.spyOn(video, "getBoundingClientRect").mockReturnValue({
    left: 100,
    top: 0,
    width: 400,
    height: 200,
    right: 500,
    bottom: 200,
    x: 100,
    y: 0,
    toJSON: () => ({}),
  });

  return {
    video,
    onActivity,
    onClick,
    onDoubleClick,
    onInteractionChange,
    onSeekRelative,
    onSeekTo,
    onSingleTap,
    onVolumeChange,
  };
}

describe("LuxPlayer touch gestures", () => {
  it("maps a horizontal swipe to a bounded time position", () => {
    expect(resolvePlayerGesture({
      startX: 100,
      startY: 300,
      currentX: 300,
      currentY: 312,
      width: 1_000,
      height: 600,
      currentTime: 10,
      duration: 100,
      volume: 0.5,
    })).toEqual({ type: "SEEK", position: 30 });

    expect(resolvePlayerGesture({
      startX: 100,
      startY: 300,
      currentX: -900,
      currentY: 300,
      width: 1_000,
      height: 600,
      currentTime: 10,
      duration: 100,
      volume: 0.5,
    })).toEqual({ type: "SEEK", position: 0 });
  });

  it("maps a vertical swipe to a bounded volume level", () => {
    expect(resolvePlayerGesture({
      startX: 500,
      startY: 500,
      currentX: 512,
      currentY: 200,
      width: 1_000,
      height: 600,
      currentTime: 10,
      duration: 100,
      volume: 0.5,
    })).toEqual({ type: "VOLUME", volume: 1 });

    expect(resolvePlayerGesture({
      startX: 500,
      startY: 100,
      currentX: 500,
      currentY: 700,
      width: 1_000,
      height: 600,
      currentTime: 10,
      duration: 100,
      volume: 0.5,
    })).toEqual({ type: "VOLUME", volume: 0 });
  });

  it("ignores taps and invalid media geometry", () => {
    const input = {
      startX: 100,
      startY: 100,
      currentX: 108,
      currentY: 106,
      width: 1_000,
      height: 600,
      currentTime: 10,
      duration: 100,
      volume: 0.5,
    };

    expect(resolvePlayerGesture(input)).toBeNull();
    expect(resolvePlayerGesture({ ...input, width: 0 })).toBeNull();
    expect(resolvePlayerGesture({ ...input, duration: 0, currentX: 400 })).toBeNull();
  });

  it("keeps touch seek and volume changes inside the video surface", () => {
    const { video, onInteractionChange, onSeekTo, onVolumeChange } = renderGestureSurface();

    dispatchPointer(video, "pointerdown", { pointerId: 1, pointerType: "touch", clientX: 200, clientY: 120 });
    dispatchPointer(video, "pointermove", { pointerId: 1, pointerType: "touch", clientX: 300, clientY: 120 });
    dispatchPointer(video, "pointerup", { pointerId: 1, pointerType: "touch", clientX: 300, clientY: 120 });

    expect(onSeekTo).toHaveBeenLastCalledWith(60);
    expect(onInteractionChange).toHaveBeenNthCalledWith(1, true);
    expect(onInteractionChange).toHaveBeenLastCalledWith(false);

    dispatchPointer(video, "pointerdown", { pointerId: 2, pointerType: "touch", clientX: 300, clientY: 140 });
    dispatchPointer(video, "pointermove", { pointerId: 2, pointerType: "touch", clientX: 304, clientY: 40 });

    expect(onVolumeChange).toHaveBeenLastCalledWith(1);
  });

  it("continues a gesture when the browser declines pointer capture", () => {
    const { video, onSeekTo } = renderGestureSurface();
    Object.defineProperty(video, "setPointerCapture", {
      configurable: true,
      value: () => {
        throw new DOMException("no active pointer", "NotFoundError");
      },
    });

    expect(() => {
      dispatchPointer(video, "pointerdown", { pointerId: 1, pointerType: "touch", clientX: 100, clientY: 100 });
    }).not.toThrow();
    dispatchPointer(video, "pointermove", { pointerId: 1, pointerType: "touch", clientX: 200, clientY: 100 });

    expect(onSeekTo).toHaveBeenLastCalledWith(60);
  });

  it("marks a touch as interactive before it moves", () => {
    const { video, onInteractionChange } = renderGestureSurface();

    dispatchPointer(video, "pointerdown", { pointerId: 1, pointerType: "touch", clientX: 100, clientY: 100 });

    expect(onInteractionChange).toHaveBeenCalledWith(true);
  });

  it("keeps the first touch as the active gesture session", () => {
    const { video, onSeekTo } = renderGestureSurface();

    dispatchPointer(video, "pointerdown", { pointerId: 1, pointerType: "touch", clientX: 100, clientY: 100 });
    dispatchPointer(video, "pointerdown", { pointerId: 2, pointerType: "touch", clientX: 300, clientY: 100 });
    dispatchPointer(video, "pointermove", { pointerId: 2, pointerType: "touch", clientX: 350, clientY: 100 });
    dispatchPointer(video, "pointermove", { pointerId: 1, pointerType: "touch", clientX: 200, clientY: 100 });

    expect(onSeekTo).toHaveBeenCalledTimes(1);
    expect(onSeekTo).toHaveBeenCalledWith(60);
  });

  it("uses the video-local half for a touch double-tap seek", () => {
    vi.useFakeTimers();
    const { video, onSeekRelative, onSingleTap } = renderGestureSurface();

    dispatchPointer(video, "pointerdown", { pointerId: 1, pointerType: "touch", clientX: 250, clientY: 100 });
    dispatchPointer(video, "pointerup", { pointerId: 1, pointerType: "touch", clientX: 250, clientY: 100 });
    dispatchPointer(video, "pointerdown", { pointerId: 2, pointerType: "touch", clientX: 250, clientY: 100 });
    dispatchPointer(video, "pointerup", { pointerId: 2, pointerType: "touch", clientX: 250, clientY: 100 });

    expect(onSeekRelative).toHaveBeenCalledWith(-10);
    act(() => vi.runAllTimers());
    expect(onSingleTap).not.toHaveBeenCalled();
  });

  it("delays one touch tap and prevents the following click from toggling twice", () => {
    vi.useFakeTimers();
    const { video, onClick, onDoubleClick, onSingleTap } = renderGestureSurface();

    dispatchPointer(video, "pointerdown", { pointerId: 1, pointerType: "touch", clientX: 300, clientY: 100 });
    dispatchPointer(video, "pointerup", { pointerId: 1, pointerType: "touch", clientX: 300, clientY: 100 });
    act(() => video.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true })));
    act(() => video.dispatchEvent(new MouseEvent("dblclick", { bubbles: true, cancelable: true })));

    expect(onClick).not.toHaveBeenCalled();
    expect(onDoubleClick).not.toHaveBeenCalled();
    act(() => vi.advanceTimersByTime(280));
    expect(onSingleTap).toHaveBeenCalledTimes(1);
    act(() => vi.advanceTimersByTime(140));
    act(() => video.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true })));
    expect(onClick).toHaveBeenCalledTimes(1);
  });

  it("ends interaction without triggering taps when the browser cancels a pointer", () => {
    vi.useFakeTimers();
    const { video, onInteractionChange, onSeekRelative, onSingleTap } = renderGestureSurface();

    dispatchPointer(video, "pointerdown", { pointerId: 1, pointerType: "touch", clientX: 200, clientY: 120 });
    dispatchPointer(video, "pointermove", { pointerId: 1, pointerType: "touch", clientX: 280, clientY: 120 });
    dispatchPointer(video, "pointercancel", { pointerId: 1, pointerType: "touch", clientX: 280, clientY: 120 });
    act(() => vi.runAllTimers());

    expect(onInteractionChange).toHaveBeenLastCalledWith(false);
    expect(onSeekRelative).not.toHaveBeenCalled();
    expect(onSingleTap).not.toHaveBeenCalled();
  });
});
