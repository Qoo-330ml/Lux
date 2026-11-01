import { describe, expect, it } from "vitest";
import {
  AUTO_PLAY_NEXT_DELAY_SECONDS,
  CREDITS_SKIP_REMAINING_SECONDS,
  autoPlayNextCountdown,
  createAutoPlayNextState,
  creditsSkipAction,
  creditsStartSeconds,
  reduceAutoPlayNext,
  shouldShowSkipCredits,
  type AutoPlayNextState,
} from "../src/features/player/player-autoplay";
import type { PlayerChapterSegment } from "../src/features/player/player-chapters";

function segment(
  start: number,
  end: number,
  markerType: string,
  chapterIndex = 0,
): PlayerChapterSegment {
  return {
    id: `chapter-${start}-${markerType}`,
    start,
    end,
    title: markerType,
    markerType,
    chapterIndex,
  };
}

function ended(enabled: boolean, nextEpisodeId: string | null): AutoPlayNextState {
  return reduceAutoPlayNext(createAutoPlayNextState(), {
    type: "ended",
    enabled,
    nextEpisodeId,
  }).state;
}

describe("LuxPlayer auto play next countdown", () => {
  it("starts a ten second countdown after the video ends with a next episode", () => {
    const transition = reduceAutoPlayNext(createAutoPlayNextState(), {
      type: "ended",
      enabled: true,
      nextEpisodeId: "episode-2",
    });

    expect(AUTO_PLAY_NEXT_DELAY_SECONDS).toBe(10);
    expect(transition).toEqual({
      state: { status: "counting", nextEpisodeId: "episode-2", secondsLeft: 10 },
      navigateTo: null,
    });
    expect(autoPlayNextCountdown(transition.state)).toEqual({
      nextEpisodeId: "episode-2",
      secondsLeft: 10,
    });
  });

  it("never offers the card for a disabled preference, a single movie, or a last episode", () => {
    for (const state of [
      ended(false, "episode-2"),
      ended(true, null),
      ended(false, null),
    ]) {
      expect(state).toEqual({ status: "idle" });
      expect(autoPlayNextCountdown(state)).toBeNull();
    }
  });

  it("navigates once the countdown reaches zero and stays idle afterwards", () => {
    let state: AutoPlayNextState = { status: "counting", nextEpisodeId: "episode-2", secondsLeft: 2 };

    const first = reduceAutoPlayNext(state, { type: "tick" });
    expect(first.state).toEqual({ status: "counting", nextEpisodeId: "episode-2", secondsLeft: 1 });
    expect(first.navigateTo).toBeNull();
    state = first.state;

    const last = reduceAutoPlayNext(state, { type: "tick" });
    expect(last).toEqual({ state: { status: "idle" }, navigateTo: "episode-2" });

    expect(reduceAutoPlayNext(last.state, { type: "tick" })).toEqual({
      state: { status: "idle" },
      navigateTo: null,
    });
  });

  it("plays the next episode immediately without waiting for the countdown", () => {
    const counting: AutoPlayNextState = {
      status: "counting",
      nextEpisodeId: "episode-2",
      secondsLeft: 7,
    };

    expect(reduceAutoPlayNext(counting, { type: "play-now" })).toEqual({
      state: { status: "idle" },
      navigateTo: "episode-2",
    });
    expect(reduceAutoPlayNext({ status: "idle" }, { type: "play-now" })).toEqual({
      state: { status: "idle" },
      navigateTo: null,
    });
  });

  it("keeps a cancelled countdown dismissed until the episode changes", () => {
    const counting: AutoPlayNextState = {
      status: "counting",
      nextEpisodeId: "episode-2",
      secondsLeft: 4,
    };

    const cancelled = reduceAutoPlayNext(counting, { type: "cancel" });
    expect(cancelled).toEqual({ state: { status: "dismissed" }, navigateTo: null });
    expect(reduceAutoPlayNext(cancelled.state, { type: "tick" }).navigateTo).toBeNull();
    expect(reduceAutoPlayNext(cancelled.state, { type: "ended", enabled: true, nextEpisodeId: "episode-2" }))
      .toEqual({ state: { status: "dismissed" }, navigateTo: null });
    expect(reduceAutoPlayNext(cancelled.state, { type: "reset" })).toEqual({
      state: { status: "idle" },
      navigateTo: null,
    });
  });

  it("drops a running countdown when the episode or the preference changes", () => {
    const counting: AutoPlayNextState = {
      status: "counting",
      nextEpisodeId: "episode-2",
      secondsLeft: 6,
    };

    expect(reduceAutoPlayNext(counting, { type: "reset" })).toEqual({
      state: { status: "idle" },
      navigateTo: null,
    });
    expect(reduceAutoPlayNext({ status: "dismissed" }, { type: "cancel" })).toEqual({
      state: { status: "dismissed" },
      navigateTo: null,
    });
  });
});

describe("LuxPlayer skip credits", () => {
  it("finds the credits start marker and ignores ordinary chapters", () => {
    expect(creditsStartSeconds([
      segment(0, 90, "CHAPTER", 0),
      segment(90, 120, "CREDITS_START", 1),
    ])).toBe(90);
    expect(creditsStartSeconds([segment(0, 120, "CHAPTER", 0)])).toBeNull();
    expect(creditsStartSeconds([])).toBeNull();
  });

  it("shows the skip action only after entering the credits chapter", () => {
    expect(shouldShowSkipCredits({ creditsStart: 90, currentTime: 89.9, duration: 120 })).toBe(false);
    expect(shouldShowSkipCredits({ creditsStart: 90, currentTime: 90, duration: 120 })).toBe(true);
    expect(shouldShowSkipCredits({ creditsStart: 90, currentTime: 119, duration: 120 })).toBe(true);
    expect(shouldShowSkipCredits({ creditsStart: 90, currentTime: 120, duration: 120 })).toBe(false);
    expect(shouldShowSkipCredits({ creditsStart: null, currentTime: 95, duration: 120 })).toBe(false);
    expect(shouldShowSkipCredits({ creditsStart: 90, currentTime: Number.NaN, duration: 120 })).toBe(false);
  });

  it("jumps to the next episode when one exists, otherwise finishes the current one", () => {
    expect(CREDITS_SKIP_REMAINING_SECONDS).toBe(1);
    expect(creditsSkipAction({ creditsStart: 90, duration: 120, hasNextEpisode: true }))
      .toEqual({ kind: "next-episode" });
    expect(creditsSkipAction({ creditsStart: 90, duration: 120, hasNextEpisode: false }))
      .toEqual({ kind: "seek", seconds: 119 });
    expect(creditsSkipAction({ creditsStart: null, duration: 120, hasNextEpisode: false })).toBeNull();
    expect(creditsSkipAction({ creditsStart: 90, duration: Number.NaN, hasNextEpisode: false })).toBeNull();
  });
});
