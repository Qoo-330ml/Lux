// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { usePlayerPlatform } from "../src/features/player/components/player-platform";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

type PlatformProps = {
  enabled?: boolean;
  playing?: boolean;
  currentTime?: number;
  duration?: number;
  onPlay?: () => void;
  onPause?: () => void;
  onSeekRelative?: (seconds: number) => void;
  onSeekTo?: (seconds: number) => void;
  onVisible?: () => void;
};

let container: HTMLDivElement | undefined;
let root: Root | undefined;
let mediaSessionDescriptor: PropertyDescriptor | undefined;
let mediaMetadataDescriptor: PropertyDescriptor | undefined;
let visibilityStateDescriptor: PropertyDescriptor | undefined;

function PlatformHarness(props: PlatformProps) {
  usePlayerPlatform({
    enabled: props.enabled ?? true,
    title: "LuxPlayer 平台测试",
    artist: "Lux",
    playing: props.playing ?? true,
    currentTime: props.currentTime ?? 30,
    duration: props.duration ?? 120,
    onPlay: props.onPlay ?? (() => undefined),
    onPause: props.onPause ?? (() => undefined),
    onSeekRelative: props.onSeekRelative ?? (() => undefined),
    onSeekTo: props.onSeekTo ?? (() => undefined),
    onVisible: props.onVisible ?? (() => undefined),
  });
  return null;
}

function renderPlatform(props: PlatformProps = {}) {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  act(() => root?.render(createElement(PlatformHarness, props)));
}

function setVisibility(value: "hidden" | "visible") {
  Object.defineProperty(document, "visibilityState", {
    configurable: true,
    value,
  });
  act(() => document.dispatchEvent(new Event("visibilitychange")));
}

afterEach(() => {
  if (root) act(() => root?.unmount());
  root = undefined;
  container?.remove();
  container = undefined;
  if (mediaSessionDescriptor) Object.defineProperty(navigator, "mediaSession", mediaSessionDescriptor);
  else delete (navigator as Navigator & { mediaSession?: unknown }).mediaSession;
  if (mediaMetadataDescriptor) Object.defineProperty(globalThis, "MediaMetadata", mediaMetadataDescriptor);
  else delete (globalThis as typeof globalThis & { MediaMetadata?: unknown }).MediaMetadata;
  if (visibilityStateDescriptor) Object.defineProperty(document, "visibilityState", visibilityStateDescriptor);
  mediaSessionDescriptor = undefined;
  mediaMetadataDescriptor = undefined;
  visibilityStateDescriptor = undefined;
  vi.restoreAllMocks();
});

describe("LuxPlayer platform integration", () => {
  it("registers Media Session playback and seek handlers with bounded position state", () => {
    mediaSessionDescriptor = Object.getOwnPropertyDescriptor(navigator, "mediaSession");
    mediaMetadataDescriptor = Object.getOwnPropertyDescriptor(globalThis, "MediaMetadata");
    const actions = new Map<string, ((details?: { seekOffset?: number; seekTime?: number }) => void) | null>();
    const mediaSession = {
      metadata: null as unknown,
      playbackState: "none",
      setActionHandler: vi.fn((action: string, handler: ((details?: { seekOffset?: number; seekTime?: number }) => void) | null) => {
        actions.set(action, handler);
      }),
      setPositionState: vi.fn(),
    };
    class TestMediaMetadata {
      constructor(readonly init: { title?: string; artist?: string }) {}
    }
    Object.defineProperty(navigator, "mediaSession", { configurable: true, value: mediaSession });
    Object.defineProperty(globalThis, "MediaMetadata", { configurable: true, value: TestMediaMetadata });
    const onPlay = vi.fn();
    const onPause = vi.fn();
    const onSeekRelative = vi.fn();
    const onSeekTo = vi.fn();

    renderPlatform({ onPlay, onPause, onSeekRelative, onSeekTo });
    actions.get("play")?.();
    actions.get("pause")?.();
    actions.get("seekbackward")?.({ seekOffset: 5 });
    actions.get("seekforward")?.();
    actions.get("seekto")?.({ seekTime: 95 });

    expect(onPlay).toHaveBeenCalledOnce();
    expect(onPause).toHaveBeenCalledOnce();
    expect(onSeekRelative).toHaveBeenNthCalledWith(1, -5);
    expect(onSeekRelative).toHaveBeenNthCalledWith(2, 10);
    expect(onSeekTo).toHaveBeenCalledWith(95);
    expect(mediaSession.playbackState).toBe("playing");
    expect(mediaSession.metadata).toBeInstanceOf(TestMediaMetadata);
    expect(mediaSession.setPositionState).toHaveBeenCalledWith({ duration: 120, position: 30 });
  });

  it("notifies LuxPlayer when the document becomes visible", () => {
    visibilityStateDescriptor = Object.getOwnPropertyDescriptor(document, "visibilityState");
    Object.defineProperty(document, "visibilityState", { configurable: true, value: "hidden" });
    const onVisible = vi.fn();
    renderPlatform({ onVisible });

    setVisibility("visible");

    expect(onVisible).toHaveBeenCalledOnce();
  });

  it("safely does nothing when Media Session is unavailable", () => {
    mediaSessionDescriptor = Object.getOwnPropertyDescriptor(navigator, "mediaSession");
    delete (navigator as Navigator & { mediaSession?: unknown }).mediaSession;

    expect(() => renderPlatform()).not.toThrow();
  });

  it("clears Media Session state when LuxPlayer unmounts", () => {
    mediaSessionDescriptor = Object.getOwnPropertyDescriptor(navigator, "mediaSession");
    const actions = new Map<string, ((details?: { seekOffset?: number; seekTime?: number }) => void) | null>();
    const mediaSession = {
      metadata: { title: "stale" } as unknown,
      playbackState: "none",
      setActionHandler: vi.fn((action: string, handler: ((details?: { seekOffset?: number; seekTime?: number }) => void) | null) => {
        actions.set(action, handler);
      }),
      setPositionState: vi.fn(),
    };
    Object.defineProperty(navigator, "mediaSession", { configurable: true, value: mediaSession });

    renderPlatform();
    act(() => root?.unmount());
    root = undefined;

    expect(mediaSession.metadata).toBeNull();
    expect(mediaSession.playbackState).toBe("none");
    expect(actions.get("play")).toBeNull();
    expect(actions.get("seekto")).toBeNull();
  });
});
