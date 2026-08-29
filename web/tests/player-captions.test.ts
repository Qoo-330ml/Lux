import { describe, expect, it } from "vitest";
import type { MediaSource } from "../src/lib/api/types";
import {
  defaultCaptionSelection,
  nativeCaptionTrack,
  overlayCaptionSource,
  playerCaptionOptions,
} from "../src/features/player/components/player-captions";

const source: MediaSource = {
  id: "source / 4k",
  streams: [
    { index: 0, type: "VIDEO", codec: "hevc" },
    { index: 2, type: "SUBTITLE", codec: "vtt", language: "zho", title: "简体中文", isExternal: true, isDefault: true },
    { index: 3, type: "SUBTITLE", codec: "srt", language: "eng", title: "English", isExternal: true },
    { index: 4, type: "SUBTITLE", codec: "vtt", language: "jpn", title: "内嵌日文", isExternal: false, isForced: true },
  ],
};

describe("LuxPlayer caption selection", () => {
  it("keeps all current-source subtitle metadata while enabling only external WebVTT", () => {
    const options = playerCaptionOptions(source, true);

    expect(options).toEqual([
      expect.objectContaining({ streamIndex: 2, label: "简体中文 · 默认", available: true }),
      expect.objectContaining({ streamIndex: 3, label: "English", available: true, format: "srt", renderMode: "overlay" }),
      expect.objectContaining({ streamIndex: 4, label: "内嵌日文 · 强制 · 暂不支持", available: false }),
    ]);
    expect(defaultCaptionSelection(options)?.streamIndex).toBe(2);
  });

  it("derives an encoded Lux subtitle URL only for a selected available track", () => {
    const selected = defaultCaptionSelection(playerCaptionOptions(source, true));

    expect(nativeCaptionTrack("item / 电影", source.id, selected)).toEqual({
      id: "caption-2",
      label: "简体中文",
      language: "zho",
      src: "/api/v1/items/item%20%2F%20%E7%94%B5%E5%BD%B1/subtitles/2?sourceId=source%20%2F%204k",
    });
    expect(nativeCaptionTrack("item", source.id, playerCaptionOptions(source, true)[1])).toBeNull();
  });

  it("uses the Lux overlay for SRT and ASS while keeping VTT native when available", () => {
    const options = playerCaptionOptions({
      id: "source-text",
      streams: [
        { index: 1, type: "SUBTITLE", codec: "ass", title: "样式字幕", isExternal: true },
        { index: 2, type: "SUBTITLE", codec: "vtt", title: "浏览器字幕", isExternal: true },
      ],
    }, true);

    expect(options).toEqual([
      expect.objectContaining({ format: "ass", renderMode: "overlay", available: true }),
      expect.objectContaining({ format: "vtt", renderMode: "native", available: true }),
    ]);
  });

  it("uses local embedded text fallback but requires an actual native track for remote STRM", () => {
    const localOptions = playerCaptionOptions({
      id: "local-mkv",
      sourceKind: "LOCAL_FILE",
      streams: [
        { index: 2, type: "SUBTITLE", codec: "subrip", title: "本地文本", isExternal: false },
        { index: 3, type: "SUBTITLE", codec: "hdmv_pgs_subtitle", title: "图形字幕", isExternal: false },
      ],
    }, true);
    expect(localOptions).toEqual([
      expect.objectContaining({ streamIndex: 2, format: "srt", renderMode: "overlay", available: true }),
      expect.objectContaining({ streamIndex: 3, available: false }),
    ]);

    const remoteOptions = playerCaptionOptions({
      id: "remote-strm",
      sourceKind: "STRM_URL",
      streams: [{ index: 2, type: "SUBTITLE", codec: "subrip", title: "远程文本", isExternal: false }],
    }, true);
    expect(remoteOptions[0]).toEqual(expect.objectContaining({ available: false }));
  });

  it("marks a matching browser-exposed in-band track as native without building a subtitle URL", () => {
    const options = playerCaptionOptions({
      id: "remote-strm",
      sourceKind: "STRM_URL",
      streams: [{ index: 2, type: "SUBTITLE", codec: "subrip", title: "远程文本", isExternal: false }],
    }, true, [{
      id: "inband-caption-0",
      label: "远程文本",
      language: "zho",
      kind: "subtitles",
      ordinal: 0,
    }]);

    expect(options[0]).toEqual(expect.objectContaining({
      available: true,
      renderMode: "native-inband",
      runtimeTrackId: "inband-caption-0",
    }));
    expect(nativeCaptionTrack("item", "remote-strm", options[0])).toBeNull();
    expect(overlayCaptionSource("item", "remote-strm", options[0])).toBeNull();
  });

  it("maps runtime tracks only across supported embedded text streams", () => {
    const options = playerCaptionOptions({
      id: "local-mkv",
      sourceKind: "LOCAL_FILE",
      streams: [
        { index: 1, type: "SUBTITLE", codec: "srt", title: "中文", isExternal: false, isDefault: true },
        { index: 2, type: "SUBTITLE", codec: "hdmv_pgs_subtitle", title: "图形字幕", isExternal: false },
        { index: 3, type: "SUBTITLE", codec: "ass", title: "English", isExternal: false },
      ],
    }, true, [
      { id: "inband-caption-0", label: "中文", kind: "subtitles", ordinal: 0 },
      { id: "inband-caption-1", label: "English", kind: "subtitles", ordinal: 1 },
    ]);

    expect(options).toEqual([
      expect.objectContaining({ streamIndex: 1, renderMode: "native-inband", runtimeTrackId: "inband-caption-0" }),
      expect.objectContaining({ streamIndex: 2, available: false }),
      expect.objectContaining({ streamIndex: 3, renderMode: "native-inband", runtimeTrackId: "inband-caption-1" }),
    ]);
  });
});
