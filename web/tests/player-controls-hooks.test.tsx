// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  usePlayerPlaybackControls,
  type PlayerPlaybackControls,
} from "../src/features/player/usePlayerPlaybackControls";
import {
  usePlayerKeyboardShortcuts,
} from "../src/features/player/usePlayerKeyboardShortcuts";
import {
  usePlayerTimelineScrubber,
  type PlayerTimelineScrubber,
} from "../src/features/player/usePlayerTimelineScrubber";
import type { LuxPlayerRuntime } from "../src/features/player/core/player-runtime";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let container: HTMLDivElement | undefined;
let root: Root | undefined;

afterEach(() => {
  vi.useRealTimers();
  if (root) act(() => root?.unmount());
  root = undefined;
  container?.remove();
  container = undefined;
  vi.restoreAllMocks();
});

function fakeVideo(options: {
  paused?: boolean;
  currentTime?: number;
  duration?: number;
} = {}) {
  const video = document.createElement("video");
  Object.defineProperties(video, {
    paused: { configurable: true, value: options.paused ?? true },
    ended: { configurable: true, value: false },
    currentTime: { configurable: true, writable: true, value: options.currentTime ?? 0 },
    duration: { configurable: true, value: options.duration ?? 100 },
    volume: { configurable: true, writable: true, value: 1 },
    muted: { configurable: true, writable: true, value: false },
    playbackRate: { configurable: true, writable: true, value: 1 },
  });
  vi.spyOn(video, "play").mockResolvedValue(undefined);
  vi.spyOn(video, "pause").mockImplementation(() => undefined);
  return video;
}

type HarnessOptions = {
  video: HTMLVideoElement;
  progressBar?: HTMLElement | null;
  runtime?: Partial<LuxPlayerRuntime> | null;
  showInfo?: boolean;
  showSettings?: boolean;
  navigate?: (delta: number) => void;
};

function mountHarness(options: HarnessOptions) {
  const state: {
    controls?: PlayerPlaybackControls;
    scrubber?: PlayerTimelineScrubber;
    currentTime: number;
    volume: number;
    seekTo: (seconds: number) => void;
    setShowInfo: ReturnType<typeof vi.fn>;
    setShowSettings: ReturnType<typeof vi.fn>;
    setIsPointerInteracting: ReturnType<typeof vi.fn>;
    isDraggingScrubberRef?: { current: boolean };
  } = {
    currentTime: 0,
    volume: 1,
    seekTo: () => undefined,
    setShowInfo: vi.fn(),
    setShowSettings: vi.fn(),
    setIsPointerInteracting: vi.fn(),
  };
  const videoRef = { current: options.video };
  const runtimeRef = { current: (options.runtime ?? null) as LuxPlayerRuntime | null };
  const progressBarRef = { current: (options.progressBar ?? null) as HTMLDivElement | null };

  function Harness() {
    const controls = usePlayerPlaybackControls({
      videoRef,
      runtimeRef,
      playerContainerRef: { current: container ?? null },
      duration: 100,
      setCurrentTime: (seconds) => {
        state.currentTime = seconds;
      },
    });
    state.controls = controls;
    const scrubber = usePlayerTimelineScrubber({
      progressBarRef,
      duration: 100,
      seekTo: controls.seekTo,
      seekRelative: controls.seekRelative,
      setIsPointerInteracting: state.setIsPointerInteracting as (value: boolean) => void,
      resetControlsTimeout: () => undefined,
    });
    state.scrubber = scrubber;
    state.isDraggingScrubberRef = scrubber.isDraggingScrubberRef;
    usePlayerKeyboardShortcuts({
      changeVolume: controls.changeVolume,
      itemId: "movie-1",
      navigate: (options.navigate ?? (() => undefined)) as never,
      resetControlsTimeout: () => undefined,
      seekRelative: controls.seekRelative,
      setShowInfo: state.setShowInfo as never,
      setShowSettings: state.setShowSettings as never,
      showInfo: options.showInfo ?? false,
      showSettings: options.showSettings ?? false,
      toggleFullscreen: controls.toggleFullscreen,
      toggleMute: controls.toggleMute,
      togglePlayPause: controls.togglePlayPause,
      volume: controls.volume,
    });
    return null;
  }

  if (root) act(() => root?.unmount());
  container?.remove();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  act(() => root?.render(<Harness />));
  return { state, videoRef, runtimeRef, progressBarRef };
}

function keydown(key: string, init: KeyboardEventInit = {}) {
  act(() => {
    window.dispatchEvent(new KeyboardEvent("keydown", { key, cancelable: true, bubbles: true, ...init }));
  });
}

describe("usePlayerPlaybackControls", () => {
  it("plays and pauses the video and clears the center splash", async () => {
    vi.useFakeTimers();
    const video = fakeVideo({ paused: true });
    const { state } = mountHarness({ video });

    await act(async () => state.controls?.togglePlayPause());
    expect(video.play).toHaveBeenCalledTimes(1);
    expect(state.controls?.centerSplash).toBe("play");
    act(() => vi.advanceTimersByTime(600));
    expect(state.controls?.centerSplash).toBeNull();

    const playing = fakeVideo({ paused: false });
    const second = mountHarness({ video: playing });
    act(() => second.state.controls?.togglePlayPause());
    expect(playing.pause).toHaveBeenCalledTimes(1);
    expect(second.state.controls?.centerSplash).toBe("pause");
  });

  it("seeks through the runtime when one is attached and clamps the target", () => {
    const video = fakeVideo({ currentTime: 0, duration: 100 });
    const seek = vi.fn();
    const { state } = mountHarness({ video, runtime: { seek } });

    act(() => state.controls?.seekTo(50));
    expect(seek).toHaveBeenLastCalledWith(50);
    expect(video.currentTime).toBe(0);

    act(() => state.controls?.seekTo(-10));
    expect(seek).toHaveBeenLastCalledWith(0);
    act(() => state.controls?.seekTo(500));
    expect(seek).toHaveBeenLastCalledWith(100);

    act(() => state.controls?.seekRelative(15));
    expect(seek).toHaveBeenLastCalledWith(15);
  });

  it("falls back to the video element without a runtime", () => {
    const video = fakeVideo({ currentTime: 12, duration: 100 });
    const { state } = mountHarness({ video, runtime: null });

    act(() => state.controls?.seekTo(30));
    expect(video.currentTime).toBe(30);
    act(() => state.controls?.seekRelative(-5));
    expect(video.currentTime).toBe(25);
  });

  it("clamps volume, mutes at zero and toggles mute", () => {
    const video = fakeVideo();
    const { state } = mountHarness({ video });

    act(() => state.controls?.changeVolume(0));
    expect(state.controls?.volume).toBe(0);
    expect(state.controls?.isMuted).toBe(true);
    expect(video.muted).toBe(true);

    act(() => state.controls?.changeVolume(1.5));
    expect(state.controls?.volume).toBe(1);
    expect(state.controls?.isMuted).toBe(false);
    expect(video.volume).toBe(1);

    act(() => state.controls?.toggleMute());
    expect(state.controls?.isMuted).toBe(true);
    expect(video.muted).toBe(true);
  });

  it("applies the playback rate to the element", () => {
    const video = fakeVideo();
    const { state } = mountHarness({ video });

    act(() => state.controls?.changePlaybackRate(1.5));
    expect(video.playbackRate).toBe(1.5);
    expect(state.controls?.playbackRate).toBe(1.5);
  });

  it("toggles fullscreen and picture-in-picture", async () => {
    const video = fakeVideo();
    const { state } = mountHarness({ video });
    let fullscreenElement: Element | null = null;
    Object.defineProperty(document, "fullscreenElement", {
      configurable: true,
      get: () => fullscreenElement,
    });
    const exitFullscreen = vi.fn(async () => {
      fullscreenElement = null;
    });
    const requestFullscreen = vi.fn(async () => {
      fullscreenElement = document.body;
    });
    Object.defineProperty(document, "exitFullscreen", { configurable: true, value: exitFullscreen });
    Object.defineProperty(HTMLElement.prototype, "requestFullscreen", {
      configurable: true,
      value: requestFullscreen,
    });
    const requestContainerFullscreen = vi.spyOn(container as HTMLDivElement, "requestFullscreen");

    await act(async () => state.controls?.toggleFullscreen());
    expect(requestContainerFullscreen).toHaveBeenCalledTimes(1);
    await act(async () => state.controls?.toggleFullscreen());
    expect(exitFullscreen).toHaveBeenCalledTimes(1);

    Object.defineProperty(document, "pictureInPictureEnabled", { configurable: true, value: true });
    let pictureInPictureElement: Element | null = null;
    Object.defineProperty(document, "pictureInPictureElement", {
      configurable: true,
      get: () => pictureInPictureElement,
    });
    const exitPictureInPicture = vi.fn(async () => {
      pictureInPictureElement = null;
    });
    Object.defineProperty(document, "exitPictureInPicture", { configurable: true, value: exitPictureInPicture });
    Object.defineProperty(video, "requestPictureInPicture", {
      configurable: true,
      value: async () => {
        pictureInPictureElement = video;
      },
    });
    const requestPictureInPicture = vi.spyOn(video, "requestPictureInPicture");
    await act(async () => state.controls?.togglePictureInPicture());
    expect(requestPictureInPicture).toHaveBeenCalledTimes(1);
    await act(async () => state.controls?.togglePictureInPicture());
    expect(exitPictureInPicture).toHaveBeenCalledTimes(1);
  });
});

describe("usePlayerKeyboardShortcuts", () => {
  it("maps the player shortcuts to playback actions", () => {
    const video = fakeVideo({ paused: true, currentTime: 20 });
    const seek = vi.fn();
    const { state } = mountHarness({ video, runtime: { seek } });

    act(() => state.controls?.togglePlayPause());
    expect(video.play).toHaveBeenCalled();

    keydown(" ");
    expect(video.play).toHaveBeenCalledTimes(2);
    keydown("ArrowLeft");
    expect(seek).toHaveBeenLastCalledWith(10);
    keydown("ArrowRight");
    expect(seek).toHaveBeenLastCalledWith(30);
    keydown("ArrowUp");
    expect(state.controls?.volume).toBe(1);
    keydown("m");
    expect(state.controls?.isMuted).toBe(true);
  });

  it("ignores typing targets", () => {
    const video = fakeVideo({ paused: true });
    mountHarness({ video });
    const input = document.createElement("input");
    document.body.append(input);
    act(() => {
      input.dispatchEvent(new KeyboardEvent("keydown", { key: " ", bubbles: true, cancelable: true }));
    });
    expect(video.play).not.toHaveBeenCalled();
    input.remove();
  });

  it("closes the info panel first on Escape", () => {
    const video = fakeVideo();
    const { state } = mountHarness({ video, showInfo: true, showSettings: true });
    keydown("Escape");
    expect(state.setShowInfo).toHaveBeenCalledWith(false);
    expect(state.setShowSettings).not.toHaveBeenCalled();
  });

  it("closes the settings panel on Escape when info is closed", () => {
    const video = fakeVideo();
    const { state } = mountHarness({ video, showInfo: false, showSettings: true });
    keydown("Escape");
    expect(state.setShowSettings).toHaveBeenCalledWith(false);
  });
});

describe("usePlayerTimelineScrubber", () => {
  function progressBar() {
    const bar = document.createElement("div");
    vi.spyOn(bar, "getBoundingClientRect").mockReturnValue({
      left: 0,
      top: 0,
      width: 100,
      height: 20,
      right: 100,
      bottom: 20,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    });
    const capture = vi.fn();
    const release = vi.fn();
    Object.defineProperty(bar, "setPointerCapture", { configurable: true, value: capture });
    Object.defineProperty(bar, "hasPointerCapture", { configurable: true, value: () => true });
    Object.defineProperty(bar, "releasePointerCapture", { configurable: true, value: release });
    return { bar, capture, release };
  }

  function pointerEvent(
    type: "pointerdown" | "pointermove" | "pointerup" | "pointercancel",
    pointerId: number,
    clientX: number,
  ) {
    const event = new Event(type, { bubbles: true, cancelable: true });
    Object.defineProperties(event, {
      pointerId: { value: pointerId },
      pointerType: { value: "touch" },
      clientX: { value: clientX },
      clientY: { value: 0 },
    });
    return event;
  }

  it("captures the pointer, seeks while dragging and commits on pointerup", () => {
    const video = fakeVideo();
    const { bar, capture, release } = progressBar();
    const seek = vi.fn();
    const { state } = mountHarness({ video, progressBar: bar, runtime: { seek } });
    const scrubber = state.scrubber;
    if (!scrubber) throw new Error("scrubber was not mounted");

    act(() => {
      scrubber.handleScrubberPointerDown({
        pointerId: 1,
        pointerType: "touch",
        button: 0,
        clientX: 50,
      } as never);
    });
    expect(capture).toHaveBeenCalledWith(1);
    expect(seek).toHaveBeenLastCalledWith(50);
    expect(state.setIsPointerInteracting).toHaveBeenLastCalledWith(true);
    expect(state.isDraggingScrubberRef?.current).toBe(true);

    act(() => {
      scrubber.handleScrubberPointerMove({ pointerId: 2, clientX: 90 } as never);
    });
    expect(seek).toHaveBeenLastCalledWith(50);

    act(() => {
      scrubber.handleScrubberPointerMove({ pointerId: 1, clientX: 80 } as never);
    });
    expect(seek).toHaveBeenLastCalledWith(80);

    act(() => {
      scrubber.finishScrubberPointer({
        pointerId: 1,
        currentTarget: bar,
        clientX: 70,
      } as never, false);
    });
    expect(release).toHaveBeenCalledWith(1);
    expect(seek).toHaveBeenLastCalledWith(80);
    expect(state.setIsPointerInteracting).toHaveBeenLastCalledWith(false);
    expect(state.isDraggingScrubberRef?.current).toBe(false);

    act(() => {
      scrubber.handleScrubberPointerDown({
        pointerId: 3,
        pointerType: "mouse",
        button: 2,
        clientX: 10,
      } as never);
    });
    expect(capture).toHaveBeenCalledTimes(1);
  });

  it("exposes hover time and percent", () => {
    const video = fakeVideo();
    const { bar } = progressBar();
    const { state } = mountHarness({ video, progressBar: bar });
    const scrubber = state.scrubber;
    if (!scrubber) throw new Error("scrubber was not mounted");

    act(() => scrubber.handleScrubberMouseMove({ clientX: 25 } as never));
    expect(state.scrubber?.hoverPercent).toBe(25);
    expect(state.scrubber?.hoverTime).toBe(25);

    act(() => scrubber.handleScrubberMouseLeave());
    expect(state.scrubber?.hoverPercent).toBeNull();
    expect(state.scrubber?.hoverTime).toBeNull();
  });

  it("handles timeline keyboard seeks", () => {
    const video = fakeVideo({ currentTime: 20 });
    const seek = vi.fn();
    const { state } = mountHarness({ video, runtime: { seek } });
    const scrubber = state.scrubber;
    if (!scrubber) throw new Error("scrubber was not mounted");

    act(() => scrubber.handleTimelineKeyDown({ key: "ArrowRight", preventDefault: vi.fn() } as never));
    expect(seek).toHaveBeenLastCalledWith(25);
    act(() => scrubber.handleTimelineKeyDown({ key: "Home", preventDefault: vi.fn() } as never));
    expect(seek).toHaveBeenLastCalledWith(0);
    act(() => scrubber.handleTimelineKeyDown({ key: "End", preventDefault: vi.fn() } as never));
    expect(seek).toHaveBeenLastCalledWith(100);
  });
});
