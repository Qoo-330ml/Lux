import { useCallback } from "react";

type PlayerNavigationOptions = {
  itemId: string;
  navigate: (to: number | string) => void;
  setShowSettings: (visible: boolean) => void;
};

/**
 * Back navigation falls back to the item page when there is no history entry,
 * and episode navigation always closes the settings panel first.
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

  const navigateToEpisode = useCallback((episodeId: string) => {
    setShowSettings(false);
    navigate(`/watch/${encodeURIComponent(episodeId)}`);
  }, [navigate, setShowSettings]);

  return { handleBack, navigateToEpisode };
}
