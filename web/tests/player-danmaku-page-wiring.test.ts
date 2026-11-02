import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

describe("LuxPlayer danmaku page wiring", () => {
  it("shows the danmaku status to the user and feeds the overlay's pause state", () => {
    // PlayerPage is the only place that can bridge the overlay status to the player surface; without
    // this wiring a failed, truncated, or empty danmaku load stays invisible.
    const source = readFileSync(
      new URL("../src/features/player/PlayerPage.tsx", import.meta.url),
      "utf8",
    );
    const overlay = /<PlayerDanmakuOverlay[\s\S]*?\/>/.exec(source)?.[0] ?? "";

    expect(overlay).toContain('itemId={itemId}');
    expect(overlay).toContain("sourceId={source?.id ?? \"\"}");
    expect(overlay).toContain("visible={danmuVisible}");
    expect(overlay).toContain("currentTime={currentTime}");
    expect(overlay).toContain("playbackRate={playbackRate}");
    expect(overlay).toContain("lifecycleKey={playbackKey}");
    expect(overlay).toContain("onStatusChange={setDanmakuStatus}");
    expect(overlay).toContain("playing={playing}");

    expect(source).toContain("const [danmakuStatus, setDanmakuStatus]");
    expect(source).toContain('className="lux-player-danmaku-status"');
    expect(source).toMatch(/\{danmakuStatus \?/);
  });
});
