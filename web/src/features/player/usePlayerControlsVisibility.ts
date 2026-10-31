import { useCallback, useEffect, useState } from "react";

const AUTO_HIDE_DELAY_MS = 3_000;

type PlayerControlsVisibilityOptions = {
  playing: boolean;
  showSettings: boolean;
  isPointerInteracting: boolean;
};

/**
 * Controls auto-hide bookkeeping. Callers only request "show again"; a single
 * effect owns the timer so concurrent mouse, touch and keyboard events cannot
 * leave stale timers behind.
 */
export function usePlayerControlsVisibility({
  playing,
  showSettings,
  isPointerInteracting,
}: PlayerControlsVisibilityOptions) {
  const [controlsVisible, setControlsVisible] = useState(true);
  const [controlActivity, setControlActivity] = useState(0);

  const resetControlsTimeout = useCallback(() => {
    setControlsVisible(true);
    setControlActivity((activity) => activity + 1);
  }, []);

  useEffect(() => {
    if (!playing || !controlsVisible || showSettings || isPointerInteracting) return undefined;
    const timeout = window.setTimeout(() => setControlsVisible(false), AUTO_HIDE_DELAY_MS);
    return () => window.clearTimeout(timeout);
  }, [controlActivity, controlsVisible, isPointerInteracting, playing, showSettings]);

  return {
    controlsVisible,
    setControlsVisible,
    controlActivity,
    setControlActivity,
    resetControlsTimeout,
  };
}
