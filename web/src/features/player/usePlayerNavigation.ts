import { useCallback } from "react";
import { autoStartPlaybackState } from "./player-autostart";

type PlayerNavigationOptions = {
  itemId: string;
  navigate: (to: number | string, options?: { replace?: boolean; state?: unknown }) => void;
  setShowSettings: (visible: boolean) => void;
};

type EpisodeNavigationOptions = {
  /** Set by the auto-play countdown, "立即播放" and skip credits only. */
  autoStart?: boolean;
};

/**
 * Back navigation falls back to the item page when there is no history entry,
 * and episode navigation always closes the settings panel first. Only the
 * automatic jumps ask for the one-shot auto start marker; the plain
 * previous/next episode buttons keep the current paused behaviour.
 */
export function usePlayerNavigation({
  itemId,
  navigate,
  setShowSettings,
}: PlayerNavigationOptions) {
  const handleBack = useCallback(() => {
    if (window.history.length > 1) {
      navigate(-1);
    } else {
      navigate(itemId ? `/items/${encodeURIComponent(itemId)}` : "/");
    }
  }, [itemId, navigate]);

  const navigateToEpisode = useCallback((
    episodeId: string,
    options?: EpisodeNavigationOptions,
  ) => {
    setShowSettings(false);
    navigate(
      `/watch/${encodeURIComponent(episodeId)}`,
      options?.autoStart ? { state: autoStartPlaybackState() } : undefined,
    );
  }, [navigate, setShowSettings]);

  return { handleBack, navigateToEpisode };
}
