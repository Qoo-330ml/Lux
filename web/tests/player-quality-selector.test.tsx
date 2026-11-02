import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { MediaSource } from "../src/lib/api/types";
import { qualityOptions } from "../src/features/player/playback-quality";
import { PlayerQualitySelector } from "../src/features/player/components/player-quality-selector";
import { PlayerSettingsPanel } from "../src/features/player/components/player-settings-panel";

const source: MediaSource = {
  id: "source-1",
  sourceKind: "LOCAL_FILE",
  streams: [
    { index: 0, type: "VIDEO", codec: "hevc", details: { Height: 2160 } },
    { index: 1, type: "AUDIO", codec: "aac" },
  ],
};

const options = qualityOptions(source, 720);

describe("PlayerQualitySelector", () => {
  it("renders the resolution tiers, the current choice and the effective status", () => {
    const markup = renderToStaticMarkup(
      <PlayerQualitySelector
        options={options}
        selectedValue="720"
        status="已缩放到 720p"
        onSelect={() => undefined}
      />,
    );

    expect(markup).toContain('id="lux-player-quality-select"');
    expect(markup).toContain('aria-label="选择画质"');
    expect(markup).toContain("画质");
    // The trigger shows the current tier; the option list itself is a portal
    // menu and is covered by the integration test.
    expect(markup).toContain('data-value="720"');
    expect(markup).toContain("720p（当前）");
    expect(markup).toContain('role="status"');
    expect(markup).toContain("已缩放到 720p");
    expect(markup).toContain("切换画质会以当前位置重建播放会话");
  });

  it("keeps the automatic label clean when no ceiling is selected", () => {
    const markup = renderToStaticMarkup(
      <PlayerQualitySelector
        options={qualityOptions(source, null)}
        selectedValue=""
        status="原画 2160p"
        onSelect={() => undefined}
      />,
    );

    // 「（当前）」 marks a resolution tier only; the automatic choice already
    // reads as 原画（自动）.
    expect(markup).toContain("原画（自动）");
    expect(markup).not.toContain("（当前）");
    expect(markup).toContain("原画 2160p");
  });

  it("stays hidden when the source has no lower tier", () => {
    const smallOptions = qualityOptions({
      ...source,
      streams: [{ index: 0, type: "VIDEO", details: { Height: 360 } }],
    }, null);
    const markup = renderToStaticMarkup(
      <PlayerQualitySelector
        options={smallOptions}
        selectedValue=""
        status="原画 360p"
        onSelect={() => undefined}
      />,
    );

    expect(markup).toBe("");
  });

  it("stays hidden for STRM sources", () => {
    const strmOptions = qualityOptions({ ...source, sourceKind: "STRM_URL" }, null);
    const markup = renderToStaticMarkup(
      <PlayerQualitySelector
        options={strmOptions}
        selectedValue=""
        status="原画（自动）"
        onSelect={() => undefined}
      />,
    );

    expect(markup).toBe("");
  });

  it("disables the selector and announces an in-flight switch", () => {
    const markup = renderToStaticMarkup(
      <PlayerQualitySelector
        options={options}
        selectedValue="720"
        switching
        status="原画（自动）"
        onSelect={() => undefined}
      />,
    );

    expect(markup).toContain("正在切换画质");
    expect(markup).toContain('disabled=""');
    expect(markup).not.toContain("切换画质会以当前位置重建播放会话");
  });

  it("locks the selector while the audio switch is running", () => {
    const markup = renderToStaticMarkup(
      <PlayerQualitySelector
        options={options}
        selectedValue="720"
        disabled
        status="已缩放到 720p"
        onSelect={() => undefined}
      />,
    );

    expect(markup).toContain('disabled=""');
    expect(markup).not.toContain("正在切换画质");
  });

  it("shows the rollback notice supplied by the player", () => {
    const markup = renderToStaticMarkup(
      <PlayerQualitySelector
        options={options}
        selectedValue=""
        notice="画质切换失败，已恢复原画。"
        status="原画（自动）"
        onSelect={() => undefined}
      />,
    );

    expect(markup).toContain("画质切换失败，已恢复原画。");
    expect(markup).not.toContain("切换画质会以当前位置重建播放会话");
  });

  it("does not call the change handler while rendering", () => {
    const onSelect = vi.fn();
    renderToStaticMarkup(
      <PlayerQualitySelector
        options={options}
        selectedValue="720"
        status="已缩放到 720p"
        onSelect={onSelect}
      />,
    );
    expect(onSelect).not.toHaveBeenCalled();
  });
});

describe("PlayerSettingsPanel quality slot", () => {
  it("renders the independent quality selector next to the other settings", () => {
    const markup = renderToStaticMarkup(
      <PlayerSettingsPanel
        quality={{
          options,
          selectedValue: "720",
          status: "已缩放到 720p",
          onSelect: () => undefined,
        }}
        onClose={() => undefined}
      />,
    );

    expect(markup).toContain('aria-label="播放设置"');
    expect(markup).toContain('aria-label="选择画质"');
    expect(markup).not.toContain('aria-label="选择字幕"');
  });

  it("omits the quality slot when the player does not supply one", () => {
    const markup = renderToStaticMarkup(
      <PlayerSettingsPanel
        onClose={() => undefined}
      />,
    );

    expect(markup).not.toContain('aria-label="选择画质"');
  });
});
