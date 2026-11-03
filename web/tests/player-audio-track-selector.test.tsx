import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { MediaSource } from "../src/lib/api/types";
import { audioTrackOptions } from "../src/features/player/audio-track-selection";
import { PlayerAudioTrackSelector } from "../src/features/player/components/player-audio-track-selector";
import { PlayerSettingsPanel } from "../src/features/player/components/player-settings-panel";

const source: MediaSource = {
  id: "source-1",
  streams: [
    { index: 0, type: "VIDEO", codec: "hevc" },
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
      language: "chi",
      codec: "eac3",
      details: { Channels: 6 },
    },
  ],
};

const tracks = audioTrackOptions(source, "2");

describe("PlayerAudioTrackSelector", () => {
  it("renders the audio tracks and marks the current selection", () => {
    const markup = renderToStaticMarkup(
      <PlayerAudioTrackSelector
        tracks={tracks}
        selectedIndex="2"
        onSelect={() => undefined}
      />,
    );

    expect(markup).toContain('role="radiogroup"');
    expect(markup).toContain('aria-checked="true"');
    expect(markup).toContain('aria-label="选择音轨"');
    expect(markup).toContain("音轨");
    expect(markup).toContain("中文");
    expect(markup).toContain("EAC3");
    expect(markup).toContain("5.1");
    expect(markup).toContain("（当前）");
    expect(markup).toContain('data-value="2"');
    expect(markup).toContain("切换音轨会以当前位置重建播放会话。");
  });

  it("stays hidden when the source has a single audio track", () => {
    const single = audioTrackOptions(
      { id: "source-2", streams: [{ index: 1, type: "AUDIO", codec: "aac" }] },
      "1",
    );
    const markup = renderToStaticMarkup(
      <PlayerAudioTrackSelector
        tracks={single}
        selectedIndex="1"
        onSelect={() => undefined}
      />,
    );

    expect(markup).toBe("");
  });

  it("disables the selector and announces an in-flight switch", () => {
    const markup = renderToStaticMarkup(
      <PlayerAudioTrackSelector
        tracks={tracks}
        selectedIndex="1"
        switching
        onSelect={() => undefined}
      />,
    );

    expect(markup).toContain('role="status"');
    expect(markup).toContain("正在切换音轨");
    expect(markup).toContain('disabled=""');
  });

  it("shows the rollback notice supplied by the player", () => {
    const markup = renderToStaticMarkup(
      <PlayerAudioTrackSelector
        tracks={tracks}
        selectedIndex="1"
        notice="音轨切换失败，已恢复原音轨。"
        onSelect={() => undefined}
      />,
    );

    expect(markup).toContain("音轨切换失败，已恢复原音轨。");
  });

  it("does not call the change handler while rendering", () => {
    const onSelect = vi.fn();
    renderToStaticMarkup(
      <PlayerAudioTrackSelector tracks={tracks} selectedIndex="1" onSelect={onSelect} />,
    );
    expect(onSelect).not.toHaveBeenCalled();
  });
});

describe("PlayerSettingsPanel audio track slot", () => {
  it("keeps audio, subtitles and speed out of the settings panel (they live in the control bar)", () => {
    const markup = renderToStaticMarkup(
      <PlayerSettingsPanel onClose={() => undefined} />,
    );

    expect(markup).toContain('aria-label="播放设置"');
    expect(markup).not.toContain('aria-label="选择音轨"');
    expect(markup).not.toContain('aria-label="选择字幕"');
    expect(markup).not.toContain("播放速度");
  });

  it("omits the audio track slot when the player does not supply one", () => {
    const markup = renderToStaticMarkup(
      <PlayerSettingsPanel
        onClose={() => undefined}
      />,
    );

    expect(markup).not.toContain('aria-label="选择音轨"');
  });
});
