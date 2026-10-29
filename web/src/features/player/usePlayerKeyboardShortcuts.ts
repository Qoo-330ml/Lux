import { useEffect, useRef } from "react";

type PlayerKeyboardShortcutOptions = {
  itemId: string;
  volume: number;
  changeVolume: (newVol: number) => void;
  seekRelative: (seconds: number) => void;
  togglePlayPause: () => void;
  toggleMute: () => void;
  toggleFullscreen: () => void;
  resetControlsTimeout: () => void;
  showInfo: boolean;
  showSettings: boolean;
  setShowInfo: (value: boolean | ((visible: boolean) => boolean)) => void;
  setShowSettings: (value: boolean | ((visible: boolean) => boolean)) => void;
  navigate: (to: number | string) => void;
};

/**
 * Global player shortcuts. Typing targets keep their own key handling, and
 * Escape unwinds overlays before it ever navigates away.
 *
 * The listener is bound once per mount and reads the latest callbacks through a
 * ref, mirroring usePlayerPlatform: playback re-renders the page roughly ten
 * times per second, so depending on callback identity here would detach and
 * reattach the window listener on every tick.
 */
export function usePlayerKeyboardShortcuts(options: PlayerKeyboardShortcutOptions) {
  const optionsRef = useRef(options);
  optionsRef.current = options;

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      if (
        target &&
        (target.tagName === "INPUT" ||
          target.tagName === "TEXTAREA" ||
          target.isContentEditable)
      ) {
        return;
      }

      const {
        itemId,
        navigate,
        resetControlsTimeout,
        setShowInfo,
        setShowSettings,
        showInfo,
        showSettings,
        volume,
        changeVolume,
        seekRelative,
        toggleFullscreen,
        toggleMute,
        togglePlayPause,
      } = optionsRef.current;

      resetControlsTimeout();

      switch (e.key) {
        case " ":
        case "k":
        case "K":
          e.preventDefault();
          togglePlayPause();
          break;
        case "ArrowLeft":
        case "j":
        case "J":
          e.preventDefault();
          seekRelative(-10);
          break;
        case "ArrowRight":
        case "l":
        case "L":
          e.preventDefault();
          seekRelative(10);
          break;
        case "ArrowUp":
          e.preventDefault();
          changeVolume(volume + 0.05);
          break;
        case "ArrowDown":
          e.preventDefault();
          changeVolume(volume - 0.05);
          break;
        case "i":
        case "I":
          e.preventDefault();
          setShowInfo((visible) => !visible);
          break;
        case "m":
        case "M":
          e.preventDefault();
          toggleMute();
          break;
        case "f":
        case "F":
          e.preventDefault();
          toggleFullscreen();
          break;
        case "Escape":
          if (showInfo) {
            e.preventDefault();
            setShowInfo(false);
          } else if (showSettings) {
            e.preventDefault();
            setShowSettings(false);
          } else if (!document.fullscreenElement) {
            e.preventDefault();
            if (window.history.length > 1) {
              navigate(-1);
            } else {
              navigate(itemId ? `/items/${encodeURIComponent(itemId)}` : "/");
            }
          }
          break;
      }
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, []);
}
