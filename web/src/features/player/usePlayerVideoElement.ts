import { useCallback, type Dispatch, type SetStateAction } from "react";
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
}: PlayerVideoElementOptions) {
  return useCallback((video: HTMLVideoElement | null) => {
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
}
