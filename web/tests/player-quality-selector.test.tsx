import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { MediaSource } from "../src/lib/api/types";
import { qualityOptions } from "../src/features/player/playback-quality";
import { PlayerQualitySelector } from "../src/features/player/components/player-quality-selector";
import { PlayerSettingsPanel } from "../src/features/player/components/player-settings-panel";

const source: MediaSource = {
  id: "source-1",
  sourceKind: "LOCAL_FILE",
  bitrate: 12_000_000,
};

const options = qualityOptions(source, 4_000_000);

describe("PlayerQualitySelector", () => {
  it("renders the tiers, the automatic choice and the effective status", () => {
    const markup = renderToStaticMarkup(
      <PlayerQualitySelector
        options={options}
        selectedValue="4000000"
        status="已限制到 4 Mbps"
        onSelect={() => undefined}
      />,
    );

    expect(markup).toContain('id="lux-player-quality-select"');
    expect(markup).toContain('aria-label="选择画质"');
    expect(markup).toContain("画质");
    // The trigger shows the current tier; the option list itself is a portal
    // menu and is covered by the integration test.
    expect(markup).toContain('data-value="4000000"');
    expect(markup).toContain("4 Mbps");
    expect(markup).toContain('role="status"');
    expect(markup).toContain("已限制到 4 Mbps");
    expect(markup).toContain("切换画质会以当前位置重建播放会话");
  });

  it("stays hidden when the source cannot be capped", () => {
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
        selectedValue="4000000"
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
        selectedValue="4000000"
        disabled
        status="已限制到 4 Mbps"
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
        selectedValue="4000000"
        status="已限制到 4 Mbps"
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
        playbackRates={[1]}
        playbackRate={1}
        onChangeRate={() => undefined}
        quality={{
          options,
          selectedValue: "4000000",
          status: "已限制到 4 Mbps",
          onSelect: () => undefined,
        }}
        onClose={() => undefined}
      />,
    );

    expect(markup).toContain('aria-label="播放设置"');
    expect(markup).toContain('aria-label="选择画质"');
    expect(markup).toContain('aria-label="选择字幕"');
  });

  it("omits the quality slot when the player does not supply one", () => {
    const markup = renderToStaticMarkup(
      <PlayerSettingsPanel
        playbackRates={[1]}
        playbackRate={1}
        onChangeRate={() => undefined}
        onClose={() => undefined}
      />,
    );

    expect(markup).not.toContain('aria-label="选择画质"');
  });
});
