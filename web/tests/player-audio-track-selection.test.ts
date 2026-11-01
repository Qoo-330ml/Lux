import { describe, expect, it } from "vitest";
import type { MediaSource } from "../src/lib/api/types";
import {
  audioParamFromQueryKey,
  audioTrackOptions,
  audioTrackSwitchParams,
  resolveConfirmedAudioTrackIndex,
  shouldShowAudioTrackSelector,
  supportsAudioTrackSwitching,
} from "../src/features/player/audio-track-selection";

function source(streams: MediaSource["streams"]): MediaSource {
  return { id: "source-1", streams };
}

const multiTrackSource = source([
  {
    index: 0,
    type: "VIDEO",
    codec: "hevc",
  },
  {
    index: 1,
    type: "AUDIO",
    language: "eng",
    codec: "aac",
    title: "English",
    isDefault: true,
    details: { Channels: 2 },
  },
  {
    index: 2,
    type: "AUDIO",
    language: "jpn",
    codec: "eac3",
    details: { Channels: "6" },
  },
  {
    index: 3,
    type: "AUDIO",
    language: "chi",
    codec: "flac",
    details: { Channels: 8 },
  },
]);

describe("player audio track selection", () => {
  it("confirms an audio index only from an explicit choice or a server session", () => {
    // 显式选择：两种会话都确认。
    expect(resolveConfirmedAudioTrackIndex(multiTrackSource, "2", "browser")).toBe("2");
    expect(resolveConfirmedAudioTrackIndex(multiTrackSource, " 3 ", "server")).toBe("3");
    // 直连且未显式选择：浏览器按容器默认轨播放，不能声称是第一条。
    expect(resolveConfirmedAudioTrackIndex(multiTrackSource, null, "browser")).toBeNull();
    expect(resolveConfirmedAudioTrackIndex(multiTrackSource, "9", "browser")).toBeNull();
    // 服务端会话未显式指定时按 0:a:0 复用第一条音轨。
    expect(resolveConfirmedAudioTrackIndex(multiTrackSource, null, "server")).toBe("1");
    expect(resolveConfirmedAudioTrackIndex(multiTrackSource, "9", "server")).toBe("1");
    expect(resolveConfirmedAudioTrackIndex(undefined, "1", "server")).toBeNull();
    expect(resolveConfirmedAudioTrackIndex(source([{ index: 0, type: "VIDEO" }]), "1", "server"))
      .toBeNull();
  });

  it("lists audio streams with language, codec, channels and the default marker", () => {
    const options = audioTrackOptions(multiTrackSource, "2");

    expect(options.map((option) => option.index)).toEqual(["1", "2", "3"]);
    expect(options[0]).toMatchObject({
      language: "eng",
      codec: "AAC",
      channels: "2.0",
      isDefault: true,
      isCurrent: false,
    });
    expect(options[0].label).toContain("英语");
    expect(options[0].label).toContain("AAC");
    expect(options[0].label).toContain("2.0");
    expect(options[0].label).toContain("默认");
    expect(options[1]).toMatchObject({
      language: "jpn",
      codec: "EAC3",
      channels: "5.1",
      isDefault: false,
      isCurrent: true,
    });
    expect(options[2]).toMatchObject({
      channels: "7.1",
      isCurrent: false,
    });
    expect(options[2].label).not.toContain("默认");
  });

  it("reads the audio selection from both playback query key layouts", () => {
    expect(audioParamFromQueryKey(["playback-bootstrap", "item", "source", "2", "7", 0])).toBe("2");
    expect(audioParamFromQueryKey(["web-playback-session", "item", "source", "2", "7", 0])).toBe("2");
    expect(audioParamFromQueryKey(["web-playback-session", "item", "source", "default", "7", 0])).toBe("default");
  });

  it("does not mark a current track when the track is unconfirmed", () => {
    expect(audioTrackOptions(multiTrackSource, null).every((option) => !option.isCurrent)).toBe(true);
    // 服务端会话未显式指定时，第一条音轨是被确认的当前音轨。
    const confirmedFirst = resolveConfirmedAudioTrackIndex(multiTrackSource, null, "server");
    expect(audioTrackOptions(multiTrackSource, confirmedFirst)[0].isCurrent).toBe(true);
  });

  it("does not offer switching for STRM sources", () => {
    expect(supportsAudioTrackSwitching({ ...multiTrackSource, sourceKind: "LOCAL_FILE" })).toBe(true);
    expect(supportsAudioTrackSwitching({ ...multiTrackSource, sourceKind: "STRM_URL" })).toBe(false);
    expect(supportsAudioTrackSwitching(undefined)).toBe(true);
  });

  it("only offers the switcher when the source has more than one audio stream", () => {
    expect(shouldShowAudioTrackSelector(audioTrackOptions(multiTrackSource, "1"))).toBe(true);
    expect(shouldShowAudioTrackSelector(audioTrackOptions(
      source([{ index: 1, type: "AUDIO", codec: "aac" }]),
      "1",
    ))).toBe(false);
    expect(shouldShowAudioTrackSelector([])).toBe(false);
  });

  it("writes the audio index to the URL and only drops an implicit first stream", () => {
    const current = new URLSearchParams({ sourceId: "source-1", subtitleStreamIndex: "7" });

    const switched = audioTrackSwitchParams(current, "2", multiTrackSource, "server");
    expect(switched.get("audioStreamIndex")).toBe("2");
    expect(switched.get("sourceId")).toBe("source-1");
    expect(switched.get("subtitleStreamIndex")).toBe("7");
    // The input parameters are not mutated.
    expect(current.get("audioStreamIndex")).toBeNull();

    // 服务端会话已按 0:a:0 复用第一条音轨，URL 保持干净。
    const backToFirst = audioTrackSwitchParams(switched, "1", multiTrackSource, "server");
    expect(backToFirst.get("audioStreamIndex")).toBeNull();
    expect(backToFirst.get("subtitleStreamIndex")).toBe("7");
  });

  it("writes the first audio stream explicitly for Direct Play", () => {
    const current = new URLSearchParams({ subtitleStreamIndex: "7" });

    // 直连时浏览器按容器默认轨播放，第一条音轨也必须显式写入才能被选中。
    const first = audioTrackSwitchParams(current, "1", multiTrackSource, "browser");
    expect(first.get("audioStreamIndex")).toBe("1");
    expect(first.get("subtitleStreamIndex")).toBe("7");
    expect(audioTrackSwitchParams(current, "3", multiTrackSource, "browser").get("audioStreamIndex"))
      .toBe("3");
    // 清除选择仍然删除参数。
    expect(audioTrackSwitchParams(first, null, multiTrackSource, "browser").get("audioStreamIndex"))
      .toBeNull();
  });

  it("keeps an explicit selection when the source exposes no streams", () => {
    const current = new URLSearchParams({ audioStreamIndex: "2" });
    expect(audioTrackSwitchParams(current, null, undefined, "server").get("audioStreamIndex"))
      .toBeNull();
  });
});
