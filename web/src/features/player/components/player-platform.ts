import { useEffect, useRef } from "react";

type MediaSessionDetails = {
  seekOffset?: number;
  seekTime?: number;
};

type MediaSessionBridge = {
  metadata: unknown;
  playbackState: "none" | "paused" | "playing";
  setActionHandler: (
    action: string,
    handler: ((details?: MediaSessionDetails) => void) | null,
  ) => void;
  setPositionState?: (state?: { duration: number; position: number }) => void;
};

export type PlayerPlatformOptions = {
  enabled: boolean;
  title: string;
  artist: string;
  playing: boolean;
  currentTime: number;
  duration: number;
  onPlay: () => void;
  onPause: () => void;
  onSeekRelative: (seconds: number) => void;
  onSeekTo: (seconds: number) => void;
  onVisible: () => void;
};

const MEDIA_ACTIONS = ["play", "pause", "seekbackward", "seekforward", "seekto"] as const;

function currentMediaSession(): MediaSessionBridge | null {
  if (typeof navigator === "undefined") return null;
  return (navigator as Navigator & { mediaSession?: MediaSessionBridge }).mediaSession ?? null;
}

function setAction(
  mediaSession: MediaSessionBridge,
  action: (typeof MEDIA_ACTIONS)[number],
  handler: (details?: MediaSessionDetails) => void,
) {
  try {
    mediaSession.setActionHandler(action, handler);
  } catch {
    // Browsers may expose Media Session but reject an individual action.
  }
}

function clearAction(mediaSession: MediaSessionBridge, action: (typeof MEDIA_ACTIONS)[number]) {
  try {
    mediaSession.setActionHandler(action, null);
  } catch {
    // An unsupported action requires no further cleanup.
  }
}

function clearMediaSessionState(mediaSession: MediaSessionBridge) {
  try {
    mediaSession.metadata = null;
  } catch {
    // The platform bridge must not block Lux playback cleanup.
  }
  try {
    mediaSession.playbackState = "none";
  } catch {
    // The platform bridge must not block Lux playback cleanup.
  }
  try {
    mediaSession.setPositionState?.();
  } catch {
    // Some browsers reject an empty position state.
  }
}

function boundedPosition(currentTime: number, duration: number) {
  if (!Number.isFinite(duration) || duration <= 0 || !Number.isFinite(currentTime)) return null;
  return { duration, position: Math.max(0, Math.min(duration, currentTime)) };
}

function finiteNumberOr(value: unknown, fallback: number) {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

/**
 * Owns browser-only platform affordances. Playback engines and Lux session
 * lifecycle remain in PlayerPage; Media Session actions only invoke its
 * explicit callbacks.
 */
export function usePlayerPlatform(options: PlayerPlatformOptions) {
  const optionsRef = useRef(options);
  optionsRef.current = options;

  useEffect(() => {
    const handleVisibilityChange = () => {
      if (document.visibilityState !== "hidden") optionsRef.current.onVisible();
    };
    document.addEventListener("visibilitychange", handleVisibilityChange);
    return () => document.removeEventListener("visibilitychange", handleVisibilityChange);
  }, []);

  useEffect(() => {
    if (!options.enabled) return;
    const mediaSession = currentMediaSession();
    if (!mediaSession) return;

    setAction(mediaSession, "play", () => optionsRef.current.onPlay());
    setAction(mediaSession, "pause", () => optionsRef.current.onPause());
    setAction(mediaSession, "seekbackward", (details) => {
      const offset = details?.seekOffset;
      optionsRef.current.onSeekRelative(-finiteNumberOr(offset, 10));
    });
    setAction(mediaSession, "seekforward", (details) => {
      const offset = details?.seekOffset;
      optionsRef.current.onSeekRelative(finiteNumberOr(offset, 10));
    });
    setAction(mediaSession, "seekto", (details) => {
      const seekTime = details?.seekTime;
      if (typeof seekTime === "number" && Number.isFinite(seekTime)) {
        optionsRef.current.onSeekTo(seekTime);
      }
    });

    return () => {
      for (const action of MEDIA_ACTIONS) clearAction(mediaSession, action);
      clearMediaSessionState(mediaSession);
    };
  }, [options.enabled]);

  useEffect(() => {
    if (!options.enabled) return;
    const mediaSession = currentMediaSession();
    if (!mediaSession) return;
    try {
      if (typeof MediaMetadata === "function") {
        mediaSession.metadata = new MediaMetadata({ title: options.title, artist: options.artist });
      }
      mediaSession.playbackState = options.playing ? "playing" : "paused";
      const position = boundedPosition(options.currentTime, options.duration);
      if (position) mediaSession.setPositionState?.(position);
    } catch {
      // Media Session is optional; it must not affect Lux playback when a
      // browser rejects metadata or position state.
    }
  }, [
    options.artist,
    options.currentTime,
    options.duration,
    options.enabled,
    options.playing,
    options.title,
  ]);

}
