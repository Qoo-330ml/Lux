import { useCallback, useEffect, useState } from "react";

const AIRPLAY_AVAILABILITY_EVENT = "webkitplaybacktargetavailabilitychanged";

type WebKitAirPlayVideo = HTMLVideoElement & {
  webkitShowPlaybackTargetPicker?: () => void;
};

type WebKitWindow = Window & {
  WebKitPlaybackTargetAvailabilityEvent?: unknown;
};

export function canUseAirPlay(video: HTMLVideoElement | null): video is WebKitAirPlayVideo {
  if (!video || typeof window === "undefined") return false;
  const webkitWindow = window as WebKitWindow;
  return Boolean(
    webkitWindow.WebKitPlaybackTargetAvailabilityEvent
      && typeof (video as WebKitAirPlayVideo).webkitShowPlaybackTargetPicker === "function",
  );
}

export function usePlayerAirPlay(video: HTMLVideoElement | null, lifecycleKey = "") {
  const [available, setAvailable] = useState(false);

  useEffect(() => {
    setAvailable(false);
    if (!canUseAirPlay(video)) return;

    const handleAvailability = (event: Event) => {
      const availability = (event as Event & { availability?: unknown }).availability;
      setAvailable(availability === "available");
    };
    video.addEventListener(AIRPLAY_AVAILABILITY_EVENT, handleAvailability);
    return () => video.removeEventListener(AIRPLAY_AVAILABILITY_EVENT, handleAvailability);
  }, [lifecycleKey, video]);

  const showPicker = useCallback(() => {
    if (!available || !canUseAirPlay(video)) return;
    try {
      video.webkitShowPlaybackTargetPicker?.();
    } catch {
      // Safari can reject the picker while the media element is transitioning.
    }
  }, [available, video]);

  return { available, showPicker };
}
