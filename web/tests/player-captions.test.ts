import { describe, expect, it } from "vitest";
import type { MediaSource } from "../src/lib/api/types";
import {
  defaultCaptionSelection,
  nativeCaptionTrack,
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
      expect.objectContaining({ streamIndex: 3, label: "English · 暂不支持", available: false }),
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
});
