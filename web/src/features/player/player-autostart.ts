/**
 * One-shot "start playing as soon as the new session is ready" marker. The
 * countdown / "立即播放" / skip-credits jump carry it in the router location state,
 * the player strips it right after mount, and a plain previous/next episode click
 * never sets it.
 */
export const AUTO_START_PLAYBACK_STATE_KEY = "luxAutoStartPlayback";

export type AutoStartPlaybackState = { [AUTO_START_PLAYBACK_STATE_KEY]: true };

export function autoStartPlaybackState(): AutoStartPlaybackState {
  return { [AUTO_START_PLAYBACK_STATE_KEY]: true };
}

export function requestsAutoStartPlayback(routeState: unknown): boolean {
  if (!routeState || typeof routeState !== "object") return false;
  return (routeState as Record<string, unknown>)[AUTO_START_PLAYBACK_STATE_KEY] === true;
}

export type AutoStartPlaybackGate = {
  /** Remember a pending auto start (idempotent). */
  request: () => void;
  /** True exactly once per request, so a later source switch cannot replay it. */
  consume: () => boolean;
};

export function createAutoStartPlaybackGate(pending: boolean): AutoStartPlaybackGate {
  let waiting = pending;
  return {
    request() {
      waiting = true;
    },
    consume() {
      if (!waiting) return false;
      waiting = false;
      return true;
    },
  };
}

export type AutoStartPlaybackOutcome = "played" | "blocked";

/** Browser autoplay policy rejections that only mean "wait for a user gesture". */
const AUTO_PLAY_POLICY_ERROR_NAMES = new Set(["NotAllowedError", "AbortError"]);

export function isAutoPlayPolicyError(error: unknown): boolean {
  if (!error || typeof error !== "object") return false;
  const name = (error as { name?: unknown }).name;
  return typeof name === "string" && AUTO_PLAY_POLICY_ERROR_NAMES.has(name);
}

/**
 * Attempts playback after an automatic episode jump. A policy rejection only
 * leaves the player paused on the normal "play" affordance; every other failure is
 * rethrown so it reaches the existing playback failure path instead of being
 * silently swallowed.
 */
export async function attemptAutoStartPlayback(
  play: () => Promise<void>,
): Promise<AutoStartPlaybackOutcome> {
  try {
    await play();
    return "played";
  } catch (error) {
    if (isAutoPlayPolicyError(error)) return "blocked";
    throw error;
  }
}

export type AutoStartPlaybackRun = AutoStartPlaybackOutcome | "skipped";

/**
 * Runs the pending one-shot request, if any, and treats it as consumed only when
 * the attempt really happened:
 * - a started playback consumes it, because the viewer now sees the new episode
 *   playing;
 * - a browser policy rejection consumes it too, because the player intentionally
 *   falls back to the manual play affordance instead of retrying;
 * - any other error re-arms the request before propagating, because that error
 *   sends the player into the server-fallback reload of the same episode, and the
 *   reloaded session is the one that must still auto start exactly once.
 */
export async function consumeAutoStartPlayback(
  gate: AutoStartPlaybackGate,
  play: () => Promise<void>,
): Promise<AutoStartPlaybackRun> {
  if (!gate.consume()) return "skipped";
  try {
    return await attemptAutoStartPlayback(play);
  } catch (error) {
    gate.request();
    throw error;
  }
}
