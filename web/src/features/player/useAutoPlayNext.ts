import { useCallback, useEffect, useRef, useState } from "react";
import {
  autoPlayNextCountdown,
  createAutoPlayNextState,
  reduceAutoPlayNext,
  type AutoPlayNextEvent,
  type AutoPlayNextState,
} from "./player-autoplay";

/** One React update per second while the countdown card is visible. */
export const AUTO_PLAY_NEXT_TICK_MS = 1_000;

type AutoPlayNextOptions = {
  playbackKey: string;
  nextEpisodeId: string | null;
  enabled: boolean;
  onNavigateNextEpisode: (episodeId: string) => void;
};

type AutoPlayNext = {
  countdown: { nextEpisodeId: string; secondsLeft: number } | null;
  handleEnded: () => void;
  playNow: () => void;
  cancel: () => void;
};

/**
 * Runs the auto-play-next countdown for the current episode. Playback events
 * only feed the pure state machine; navigation and the once-per-second timer
 * live here so PlayerPage stays declarative.
 */
export function useAutoPlayNext({
  playbackKey,
  nextEpisodeId,
  enabled,
  onNavigateNextEpisode,
}: AutoPlayNextOptions): AutoPlayNext {
  const [state, setState] = useState<AutoPlayNextState>(createAutoPlayNextState);
  const stateRef = useRef(state);
  const navigateRef = useRef(onNavigateNextEpisode);

  useEffect(() => {
    navigateRef.current = onNavigateNextEpisode;
  }, [onNavigateNextEpisode]);

  const apply = useCallback((event: AutoPlayNextEvent) => {
    const transition = reduceAutoPlayNext(stateRef.current, event);
    stateRef.current = transition.state;
    setState(transition.state);
    if (transition.navigateTo) navigateRef.current(transition.navigateTo);
  }, []);

  // A different episode (or source/attempt) never inherits a pending countdown.
  useEffect(() => {
    apply({ type: "reset" });
  }, [apply, playbackKey]);

  // Turning the preference off must also retract a card that is already counting.
  useEffect(() => {
    if (!enabled) apply({ type: "reset" });
  }, [apply, enabled]);

  useEffect(() => {
    if (state.status !== "counting") return undefined;
    const timer = window.setInterval(() => apply({ type: "tick" }), AUTO_PLAY_NEXT_TICK_MS);
    return () => window.clearInterval(timer);
  }, [apply, state.status]);

  const handleEnded = useCallback(() => {
    apply({ type: "ended", enabled, nextEpisodeId });
  }, [apply, enabled, nextEpisodeId]);
  const playNow = useCallback(() => apply({ type: "play-now" }), [apply]);
  const cancel = useCallback(() => apply({ type: "cancel" }), [apply]);

  return { countdown: autoPlayNextCountdown(state), handleEnded, playNow, cancel };
}
