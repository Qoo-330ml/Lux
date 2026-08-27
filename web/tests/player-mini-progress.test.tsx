import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { PlayerMiniProgressBar } from "../src/features/player/components/player-mini-progress";

describe("LuxPlayer hidden-state mini progress", () => {
  it("renders played and buffered proportions without an interactive surface", () => {
    const markup = renderToStaticMarkup(
      <PlayerMiniProgressBar
        controlsVisible={false}
        currentTime={25}
        duration={100}
        bufferedEnd={60}
      />,
    );

    expect(markup).toContain('data-lux-player-mini-progress="true"');
    expect(markup).toContain('aria-hidden="true"');
    expect(markup).toContain('class="lux-player-mini-progress-buffered" style="width:60%"');
    expect(markup).toContain('class="lux-player-mini-progress-played" style="width:25%"');
    expect(markup).not.toContain("tabindex");
  });

  it("stays absent while controls are visible or the media is live", () => {
    expect(renderToStaticMarkup(
      <PlayerMiniProgressBar controlsVisible currentTime={25} duration={100} bufferedEnd={60} />,
    )).toBe("");
    expect(renderToStaticMarkup(
      <PlayerMiniProgressBar controlsVisible={false} currentTime={25} duration={Number.POSITIVE_INFINITY} bufferedEnd={60} />,
    )).toBe("");
  });
});
