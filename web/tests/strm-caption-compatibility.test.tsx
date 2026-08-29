// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  overlayCaptionSource,
  playerCaptionOptions,
  nativeCaptionTrack,
  type PlayerOverlayCaptionSource,
} from "../src/features/player/components/player-captions";
import { PlayerCaptionOverlay } from "../src/features/player/components/player-caption-overlay";
import { runSingleMediaReadCaptionExperiment } from "../src/features/player/single-media-read-caption-experiment";
import type { MediaSource } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const localSource: MediaSource = {
  id: "local-fixed-mkv",
  sourceKind: "LOCAL_FILE",
  container: "mkv",
  streams: [
    { index: 1, type: "VIDEO", codec: "h264" },
    { index: 2, type: "SUBTITLE", codec: "subrip", language: "zho", title: "中文", isExternal: false, isDefault: true },
    { index: 3, type: "SUBTITLE", codec: "ass", language: "eng", title: "English", isExternal: false },
    { index: 4, type: "SUBTITLE", codec: "hdmv_pgs_subtitle", language: "zho", title: "PGS 图形字幕", isExternal: false },
    { index: 5, type: "SUBTITLE", codec: "sup", language: "zho", title: "SUP 图形字幕", isExternal: false },
    { index: 6, type: "SUBTITLE", codec: "ssa", language: "jpn", title: "SSA", isExternal: false },
  ],
};

const remoteUrlSource: MediaSource = {
  id: "remote-url-strm",
  sourceKind: "STRM_URL",
  externalUrl: "https://fixture.invalid/media/movie.mkv",
  streams: [
    { index: 1, type: "VIDEO", codec: "h264" },
    { index: 2, type: "SUBTITLE", codec: "subrip", language: "zho", title: "远程中文", isExternal: false },
    { index: 3, type: "SUBTITLE", codec: "hdmv_pgs_subtitle", title: "远程 PGS", isExternal: false },
  ],
};

const remotePathSource: MediaSource = {
  ...remoteUrlSource,
  id: "remote-path-strm",
  externalUrl: "/fixture/cloud/movie.mkv",
};

function localOverlay(streamIndex: number, source = localSource): PlayerOverlayCaptionSource {
  const option = playerCaptionOptions(source, true).find((entry) => entry.streamIndex === streamIndex);
  const overlay = overlayCaptionSource("fixed-item", source.id, option);
  if (!overlay) throw new Error("expected a selectable local text caption");
  return overlay;
}

describe("LUX-229 local and remote STRM caption compatibility gate", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;

  beforeEach(() => {
    vi.stubGlobal("Worker", undefined);
  });

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
    root = undefined;
    container = undefined;
    vi.unstubAllGlobals();
  });

  it("keeps local embedded SRT/ASS selectable and reports PGS as unsupported", () => {
    const options = playerCaptionOptions(localSource, true);

    expect(options).toEqual([
      expect.objectContaining({ streamIndex: 2, format: "srt", renderMode: "overlay", available: true }),
      expect.objectContaining({ streamIndex: 3, format: "ass", renderMode: "overlay", available: true }),
      expect.objectContaining({ streamIndex: 4, available: false, unavailableReason: "当前不支持此字幕格式" }),
      expect.objectContaining({ streamIndex: 5, available: false, unavailableReason: "当前不支持此字幕格式" }),
      expect.objectContaining({ streamIndex: 6, format: "ssa", renderMode: "overlay", available: true }),
    ]);
    expect(overlayCaptionSource("fixed-item", localSource.id, options[0])).toEqual(
      expect.objectContaining({ src: "/api/v1/items/fixed-item/subtitles/2?sourceId=local-fixed-mkv" }),
    );
  });

  it("keeps URL and path STRM media direct while allowing only an actual native track", () => {
    for (const source of [remoteUrlSource, remotePathSource]) {
      const withoutNative = playerCaptionOptions(source, true);
      expect(withoutNative[0]).toEqual(expect.objectContaining({ available: false }));
      expect(overlayCaptionSource("fixed-item", source.id, withoutNative[0])).toBeNull();
      expect(nativeCaptionTrack("fixed-item", source.id, withoutNative[0])).toBeNull();
      expect(withoutNative[1]).toEqual(expect.objectContaining({ available: false }));

      const withNative = playerCaptionOptions(source, true, [{
        id: "native-caption-0",
        label: "远程中文",
        language: "zho",
        kind: "subtitles",
        ordinal: 0,
      }]);
      expect(withNative[0]).toEqual(expect.objectContaining({
        available: true,
        renderMode: "native-inband",
        runtimeTrackId: "native-caption-0",
      }));
      expect(overlayCaptionSource("fixed-item", source.id, withNative[0])).toBeNull();
      expect(nativeCaptionTrack("fixed-item", source.id, withNative[0])).toBeNull();
    }
  });

  it("loads local text only after selection and clears it on seek, source change, fallback, and leave", async () => {
    const responses = new Map<string, string>([
      ["/api/v1/items/fixed-item/subtitles/2?sourceId=local-fixed-mkv", "1\n00:00:00,000 --> 00:00:02,000\n本地 SRT 字幕"],
      ["/api/v1/items/fixed-item/subtitles/3?sourceId=local-fixed-mkv", "[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:00.00,0:00:02.00,Default,,,,,,切换后的 ASS 字幕"],
    ]);
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const key = String(input);
      const text = responses.get(key);
      if (!text) throw new Error(`unexpected caption request: ${key}`);
      return {
        ok: true,
        headers: new Headers({ "content-length": String(new TextEncoder().encode(text).byteLength) }),
        body: null,
        text: async () => text,
      } as Response;
    });
    vi.stubGlobal("fetch", fetchMock);

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const srt = localOverlay(2);
    const ass = localOverlay(3);

    await act(async () => {
      root?.render(
        <PlayerCaptionOverlay source={null} currentTime={1} lifecycleKey="none" />,
      );
    });
    expect(fetchMock).not.toHaveBeenCalled();

    await act(async () => {
      root?.render(
        <PlayerCaptionOverlay source={srt} currentTime={1} lifecycleKey="local-fixed-mkv" />,
      );
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(container.querySelector(".lux-player-caption-text")?.textContent).toBe("本地 SRT 字幕");
    expect(fetchMock).toHaveBeenCalledTimes(1);

    await act(async () => {
      root?.render(
        <PlayerCaptionOverlay source={srt} currentTime={3} lifecycleKey="local-fixed-mkv" />,
      );
    });
    expect(container.querySelector(".lux-player-caption-overlay")).toBeNull();

    await act(async () => {
      root?.render(
        <PlayerCaptionOverlay source={ass} currentTime={1} lifecycleKey="local-fixed-mkv-ass" />,
      );
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(container.querySelector(".lux-player-caption-text")?.textContent).toBe("切换后的 ASS 字幕");
    expect(container.textContent).not.toContain("本地 SRT 字幕");

    await act(async () => {
      root?.render(
        <PlayerCaptionOverlay source={null} currentTime={1} lifecycleKey="fallback" />,
      );
    });
    expect(container.querySelector(".lux-player-caption-overlay")).toBeNull();

    await act(async () => root?.unmount());
    root = undefined;
    expect(container.textContent).toBe("");
  });

  it("keeps the direct playback path when the single-read experiment is disabled or fails", async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
    const response = new Response(null, {
      status: 206,
      headers: {
        "content-type": "video/x-matroska",
        "accept-ranges": "bytes",
        "content-range": "bytes 0-2/3",
      },
    });
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new Uint8Array([1, 2, 3]));
        controller.close();
      },
    });

    const disabled = await runSingleMediaReadCaptionExperiment({
      enabled: false,
      sourceKind: "STRM_URL",
      response,
      body,
      request: { mode: "cors", rangeRequested: true },
      parser: () => "unused",
    });
    expect(disabled).toMatchObject({ status: "skipped", reason: "disabled", bytesRead: 0 });

    const failed = await runSingleMediaReadCaptionExperiment({
      enabled: true,
      sourceKind: "STRM_URL",
      response: new Response(null, {
        status: 206,
        headers: {
          "content-type": "video/x-matroska",
          "accept-ranges": "bytes",
          "content-range": "bytes 0-2/3",
        },
      }),
      body: new ReadableStream<Uint8Array>({
        start(controller) {
          controller.enqueue(new Uint8Array([1, 2, 3]));
          controller.close();
        },
      }),
      request: { mode: "cors", rangeRequested: true },
      parser: () => { throw new Error("fixture parser failure"); },
    });
    expect(failed).toMatchObject({ status: "failed", reason: "parser-failed", bytesRead: 3 });
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
