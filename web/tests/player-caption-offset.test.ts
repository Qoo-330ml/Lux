import { describe, expect, it } from "vitest";
import {
  CAPTION_OFFSET_MAX,
  CAPTION_OFFSET_MIN,
  createNativeCaptionOffsetController,
  formatCaptionOffset,
  normalizeCaptionOffset,
  offsetCaptionCues,
} from "../src/features/player/caption-offset";

describe("LuxPlayer caption offset", () => {
  it("normalizes the setting to the bounded tenth-second range", () => {
    expect(normalizeCaptionOffset(Number.NaN)).toBe(0);
    expect(normalizeCaptionOffset(-10.04)).toBe(CAPTION_OFFSET_MIN);
    expect(normalizeCaptionOffset(10.04)).toBe(CAPTION_OFFSET_MAX);
    expect(normalizeCaptionOffset(1.26)).toBe(1.3);
    expect(formatCaptionOffset(-1.2)).toBe("-1.2s");
    expect(formatCaptionOffset(0)).toBe("0.0s");
    expect(formatCaptionOffset(1.2)).toBe("+1.2s");
  });

  it("shifts overlay cues from original timing and clips them to the media duration", () => {
    const cues = [
      { id: "cue-0", start: 0.5, end: 2, text: "one" },
      { id: "cue-1", start: 4.5, end: 7, text: "two" },
      { id: "cue-2", start: 8, end: 9, text: "outside" },
    ];

    expect(offsetCaptionCues(cues, 1, 6)).toEqual([
      { id: "cue-0", start: 1.5, end: 3, text: "one" },
      { id: "cue-1", start: 5.5, end: 6, text: "two" },
    ]);
    expect(offsetCaptionCues(cues, -1, 6)[0]).toMatchObject({ start: 0, end: 1 });
    expect(offsetCaptionCues(cues, 1, 6)).toEqual(offsetCaptionCues(cues, 1, 6));
    expect(cues[0]).toMatchObject({ start: 0.5, end: 2 });
  });

  it("reapplies native cue timing from the original values and restores on cleanup", () => {
    const first = { startTime: 1, endTime: 3 };
    const second = { startTime: 4, endTime: 6 };
    const cues: Array<typeof first> = [first, second];
    const track = {
      cues,
      addCue(cue: typeof first) {
        if (!cues.includes(cue)) cues.push(cue);
      },
      removeCue(cue: typeof first) {
        const index = cues.indexOf(cue);
        if (index >= 0) cues.splice(index, 1);
      },
    } as unknown as TextTrack;
    const controller = createNativeCaptionOffsetController(track);

    controller.apply(1, 10);
    expect(first).toMatchObject({ startTime: 2, endTime: 4 });
    expect(second).toMatchObject({ startTime: 5, endTime: 7 });

    controller.apply(-1, 5);
    expect(first).toMatchObject({ startTime: 0, endTime: 2 });
    expect(second).toMatchObject({ startTime: 3, endTime: 5 });

    controller.restore();
    expect(first).toMatchObject({ startTime: 1, endTime: 3 });
    expect(second).toMatchObject({ startTime: 4, endTime: 6 });
  });
});
