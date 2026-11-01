import type { PlayerChapterSegment } from "./player-chapters";

/** Countdown shown before the player moves on to the next episode by itself. */
export const AUTO_PLAY_NEXT_DELAY_SECONDS = 10;

/**
 * Credits markers have no matching end marker, so their range always runs to the
 * media end. "Skip credits" therefore parks the playhead just short of the end so
 * the media finishes by itself instead of pretending to know where credits stop.
 */
export const CREDITS_SKIP_REMAINING_SECONDS = 1;

export type AutoPlayNextState =
  | { status: "idle" }
  | { status: "counting"; nextEpisodeId: string; secondsLeft: number }
  | { status: "dismissed" };

export type AutoPlayNextEvent =
  | { type: "ended"; enabled: boolean; nextEpisodeId: string | null }
  | { type: "tick" }
  | { type: "play-now" }
  | { type: "cancel" }
  | { type: "reset" };

export type AutoPlayNextTransition = {
  state: AutoPlayNextState;
  /** Episode id the page should navigate to right now, if any. */
  navigateTo: string | null;
};

const IDLE: AutoPlayNextState = { status: "idle" };
const DISMISSED: AutoPlayNextState = { status: "dismissed" };

/**
 * Countdown state machine for "自动连播下一集". The reducer stays pure: the page
 * performs the navigation it receives in `navigateTo`.
 */
export function reduceAutoPlayNext(
  state: AutoPlayNextState,
  event: AutoPlayNextEvent,
): AutoPlayNextTransition {
  switch (event.type) {
    case "ended": {
      if (!event.enabled || !event.nextEpisodeId) return { state: IDLE, navigateTo: null };
      // A cancelled card stays cancelled for this episode; a new episode resets first.
      if (state.status === "dismissed") return { state: DISMISSED, navigateTo: null };
      return {
        state: {
          status: "counting",
          nextEpisodeId: event.nextEpisodeId,
          secondsLeft: AUTO_PLAY_NEXT_DELAY_SECONDS,
        },
        navigateTo: null,
      };
    }
    case "tick": {
      if (state.status !== "counting") return { state, navigateTo: null };
      if (state.secondsLeft > 1) {
        return {
          state: { ...state, secondsLeft: state.secondsLeft - 1 },
          navigateTo: null,
        };
      }
      return { state: IDLE, navigateTo: state.nextEpisodeId };
    }
    case "play-now": {
      if (state.status !== "counting") return { state, navigateTo: null };
      return { state: IDLE, navigateTo: state.nextEpisodeId };
    }
    case "cancel":
      return state.status === "counting"
        ? { state: DISMISSED, navigateTo: null }
        : { state, navigateTo: null };
    case "reset":
      return { state: IDLE, navigateTo: null };
  }
}

export function createAutoPlayNextState(): AutoPlayNextState {
  return IDLE;
}

/** Rendered countdown payload, or null when the card must stay hidden. */
export function autoPlayNextCountdown(
  state: AutoPlayNextState,
): { nextEpisodeId: string; secondsLeft: number } | null {
  return state.status === "counting"
    ? { nextEpisodeId: state.nextEpisodeId, secondsLeft: state.secondsLeft }
    : null;
}

export function creditsStartSeconds(
  segments: readonly PlayerChapterSegment[],
): number | null {
  const segment = segments.find((entry) => entry.markerType === "CREDITS_START");
  if (!segment || !Number.isFinite(segment.start) || segment.start < 0) return null;
  return segment.start;
}

export function shouldShowSkipCredits(input: {
  creditsStart: number | null;
  currentTime: number;
  duration: number;
}): boolean {
  const { creditsStart, currentTime, duration } = input;
  if (creditsStart === null || !Number.isFinite(currentTime)) return false;
  if (currentTime < creditsStart) return false;
  if (Number.isFinite(duration) && duration > 0 && currentTime >= duration) return false;
  return true;
}

export type CreditsSkipAction =
  | { kind: "next-episode" }
  | { kind: "seek"; seconds: number };

/**
 * "Skip credits" reuses the plain episode navigation when a next episode exists;
 * for the last episode (or a movie) it seeks to just before the media end.
 */
export function creditsSkipAction(input: {
  creditsStart: number | null;
  duration: number;
  hasNextEpisode: boolean;
}): CreditsSkipAction | null {
  if (input.creditsStart === null) return null;
  if (input.hasNextEpisode) return { kind: "next-episode" };
  if (!Number.isFinite(input.duration) || input.duration <= 0) return null;
  const seconds = Math.max(
    input.creditsStart,
    input.duration - CREDITS_SKIP_REMAINING_SECONDS,
  );
  return { kind: "seek", seconds };
}
