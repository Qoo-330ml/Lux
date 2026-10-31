// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { MediaItem } from "../src/lib/api/types";
import { usePlayerScreenshot, type PlayerScreenshot } from "../src/features/player/usePlayerScreenshot";

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

const MEDIA: MediaItem = { id: "movie-1", title: "截图: 测试/媒体", itemType: "MOVIE" };

function mountHarness(media: MediaItem | undefined, video: HTMLVideoElement | null) {
  const state: { screenshot?: PlayerScreenshot } = {};
  function Harness() {
    state.screenshot = usePlayerScreenshot({ videoRef: { current: video }, media });
    return null;
  }
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  act(() => root?.render(<Harness />));
  return state;
}

describe("usePlayerScreenshot", () => {
  it("reports an unavailable frame for a video without pixels", () => {
    const video = document.createElement("video");
    const state = mountHarness(MEDIA, video);

    act(() => state.screenshot?.takeScreenshot());
    expect(state.screenshot?.screenshotStatus).toBe("当前视频帧尚未就绪");
  });

  it("reports an unavailable frame when no video is attached", () => {
    const state = mountHarness(MEDIA, null);
    act(() => state.screenshot?.takeScreenshot());
    expect(state.screenshot?.screenshotStatus).toBe("当前视频帧尚未就绪");
  });

  it("saves a PNG download and clears the status after 2.5 seconds", () => {
    vi.useFakeTimers();
    const video = document.createElement("video");
    Object.defineProperties(video, {
      videoWidth: { configurable: true, value: 640 },
      videoHeight: { configurable: true, value: 360 },
    });
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({
      drawImage: vi.fn(),
    } as unknown as CanvasRenderingContext2D);
    vi.spyOn(HTMLCanvasElement.prototype, "toBlob").mockImplementation((callback) => {
      callback(new Blob(["fixture"], { type: "image/png" }));
    });
    const createObjectURL = vi.fn(() => "blob:lux-screenshot");
    const revokeObjectURL = vi.fn();
    vi.stubGlobal("URL", { createObjectURL, revokeObjectURL });
    let fileName = "";
    vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function click() {
      fileName = this.download;
    });
    const state = mountHarness(MEDIA, video);

    act(() => state.screenshot?.takeScreenshot());
    expect(fileName).toMatch(/^截图- 测试-媒体-.*\.png$/);
    expect(state.screenshot?.screenshotStatus).toBe("截图已保存");

    act(() => vi.advanceTimersByTime(2_500));
    expect(state.screenshot?.screenshotStatus).toBeNull();
  });
});
