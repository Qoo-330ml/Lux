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

/**
 * Attempts playback without treating a browser autoplay rejection as a failure:
 * the player stays paused on the normal "play" affordance instead of reporting an
 * error the user cannot act on.
 */
export async function attemptAutoStartPlayback(
  play: () => Promise<void>,
): Promise<AutoStartPlaybackOutcome> {
  try {
    await play();
    return "played";
  } catch {
    return "blocked";
  }
}
