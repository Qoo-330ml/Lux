import { useCallback, useState, type Dispatch, type SetStateAction } from "react";
import { NativeVideoEngine, type PlaybackEngine } from "./playback-engine";
import type { LuxPlayerRuntime } from "./core/player-runtime";
import type { PlayerRuntimeCaptionTrack } from "./components/player-captions";

type PlayerVideoElementOptions = {
  videoRef: { current: HTMLVideoElement | null };
  lastVideoRef: { current: HTMLVideoElement | null };
  engineRef: { current: PlaybackEngine | null };
  runtimeRef: { current: LuxPlayerRuntime | null };
  setAirPlayVideo: (video: HTMLVideoElement | null) => void;
  setNativeCaptionTracks: Dispatch<SetStateAction<PlayerRuntimeCaptionTrack[]>>;
};

export type BoundPlayerVideoElement = {
  /** Callback ref for the rendered `<video>`; also binds the engine refs below. */
  setVideoRef: (video: HTMLVideoElement | null) => void;
  /**
   * The element currently in the DOM, as React state.
   *
   * The playback runtime can only load a plan once a `<video>` exists, and the page replaces that
   * element whenever it renders a loading state (a replacement session, a bootstrap retry). A ref
   * alone cannot re-trigger an effect when the element comes back, so the mounted element is
   * mirrored into state: the runtime effect depends on it and reloads as soon as it is attached.
   */
  videoElement: HTMLVideoElement | null;
};

/**
 * Binds the rendered video element to the engine refs. A null element tears the
 * runtime down and clears the per-element caption/airplay state.
 */
export function usePlayerVideoElement({
  videoRef,
  lastVideoRef,
  engineRef,
  runtimeRef,
  setAirPlayVideo,
  setNativeCaptionTracks,
}: PlayerVideoElementOptions): BoundPlayerVideoElement {
  const [videoElement, setVideoElement] = useState<HTMLVideoElement | null>(null);
  const setVideoRef = useCallback((video: HTMLVideoElement | null) => {
    // The state update is what re-runs the playback runtime effect once these refs are bound;
    // re-attaching the same element is a no-op because `Object.is` equal state bails out.
    setVideoElement(video);
    if (!video) {
      const runtime = runtimeRef.current;
      runtimeRef.current = null;
      if (runtime?.element) {
        runtime.destroy();
      } else {
        engineRef.current?.destroy();
      }
      engineRef.current = null;
      videoRef.current = null;
      setNativeCaptionTracks([]);
      setAirPlayVideo(null);
      return;
    }
    videoRef.current = video;
    lastVideoRef.current = video;
    setAirPlayVideo(video);
    engineRef.current = new NativeVideoEngine(video);
  }, [engineRef, lastVideoRef, runtimeRef, setAirPlayVideo, setNativeCaptionTracks, videoRef]);
  return { setVideoRef, videoElement };
}
