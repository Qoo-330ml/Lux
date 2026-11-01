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
  /** True while a request is still waiting, without consuming it. */
  peek: () => boolean;
  /** True exactly once per request, so a later source switch cannot replay it. */
  consume: () => boolean;
};

export function createAutoStartPlaybackGate(pending: boolean): AutoStartPlaybackGate {
  let waiting = pending;
  return {
    request() {
      waiting = true;
    },
    peek() {
      return waiting;
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

/**
 * Settles the pending one-shot request after a failed automatic start attempt.
 *
 * The request is kept only when the same episode really reloads — the server
 * fallback plan or a Direct Play candidate retry — because that reloaded session
 * is the one that must still auto start once. Every terminal failure (remote
 * source, an already-used fallback attempt, an unsupported plan) clears it, so a
 * later manual session such as the next episode, an audio track switch or a
 * quality switch does not inherit the request and start playing on its own.
 */
export function settleAutoStartPlaybackAfterFailure(
  gate: AutoStartPlaybackGate,
  reloadPending: boolean,
): void {
  if (!reloadPending) gate.consume();
}
