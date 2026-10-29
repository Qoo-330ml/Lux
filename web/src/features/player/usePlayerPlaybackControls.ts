import { useCallback, useEffect, useRef, useState } from "react";
import type { LuxPlayerRuntime } from "./core/player-runtime";

type VideoRef = { current: HTMLVideoElement | null };

export type PlayerPlaybackControls = {
  volume: number;
  isMuted: boolean;
  playbackRate: number;
  isFullscreen: boolean;
  centerSplash: "play" | "pause" | null;
  togglePlayPause: () => void;
  seekTo: (seconds: number) => void;
  seekRelative: (seconds: number) => void;
  toggleMute: () => void;
  changeVolume: (newVol: number) => void;
  changePlaybackRate: (rate: number) => void;
  toggleFullscreen: () => void;
  togglePictureInPicture: () => void;
};

type PlayerPlaybackControlsOptions = {
  videoRef: VideoRef;
  runtimeRef: { current: LuxPlayerRuntime | null };
  playerContainerRef: { current: HTMLDivElement | null };
  duration: number;
  setCurrentTime: (seconds: number) => void;
};

/**
 * Transport controls shared by the on-screen chrome, gestures, the timeline and
 * keyboard shortcuts. Volume/rate/fullscreen state lives here so every trigger
 * observes the same values.
 */
export function usePlayerPlaybackControls({
  videoRef,
  runtimeRef,
  playerContainerRef,
  duration,
  setCurrentTime,
}: PlayerPlaybackControlsOptions): PlayerPlaybackControls {
  const [volume, setVolume] = useState(1);
  const [isMuted, setIsMuted] = useState(false);
  const [playbackRate, setPlaybackRate] = useState(1.0);
  const [isFullscreen, setIsFullscreen] = useState(false);
  const [centerSplash, setCenterSplash] = useState<"play" | "pause" | null>(null);
  const splashTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    const handleFullscreenChange = () => {
      setIsFullscreen(Boolean(document.fullscreenElement));
    };
    document.addEventListener("fullscreenchange", handleFullscreenChange);
    return () => {
      document.removeEventListener("fullscreenchange", handleFullscreenChange);
    };
  }, []);

  useEffect(() => () => {
    if (splashTimeoutRef.current) clearTimeout(splashTimeoutRef.current);
  }, []);

  const showCenterSplash = (type: "play" | "pause") => {
    setCenterSplash(type);
    if (splashTimeoutRef.current) clearTimeout(splashTimeoutRef.current);
    splashTimeoutRef.current = setTimeout(() => {
      setCenterSplash(null);
    }, 600);
  };

  const togglePlayPause = useCallback(() => {
    const video = videoRef.current;
    if (!video) return;
    if (video.paused) {
      void video.play().then(() => {
        showCenterSplash("play");
      });
    } else {
      video.pause();
      showCenterSplash("pause");
    }
  }, [videoRef]);

  const seekTo = useCallback((seconds: number) => {
    const video = videoRef.current;
    if (!video) return;
    const maximum = Number.isFinite(video.duration) && video.duration > 0
      ? video.duration
      : Math.max(0, duration);
    const target = Math.max(0, Math.min(maximum, seconds));
    if (runtimeRef.current) runtimeRef.current.seek(target);
    else video.currentTime = target;
    setCurrentTime(target);
  }, [duration, runtimeRef, setCurrentTime, videoRef]);

  const seekRelative = useCallback((seconds: number) => {
    const video = videoRef.current;
    if (!video) return;
    seekTo(video.currentTime + seconds);
  }, [seekTo, videoRef]);

  const toggleMute = useCallback(() => {
    const video = videoRef.current;
    if (!video) return;
    const nextMuted = !video.muted;
    video.muted = nextMuted;
    setIsMuted(nextMuted);
  }, [videoRef]);

  const changeVolume = useCallback((newVol: number) => {
    const video = videoRef.current;
    if (!video) return;
    const bounded = Math.max(0, Math.min(1, newVol));
    video.volume = bounded;
    video.muted = bounded === 0;
    setVolume(bounded);
    setIsMuted(bounded === 0);
  }, [videoRef]);

  const toggleFullscreen = useCallback(() => {
    const container = playerContainerRef.current;
    if (!container) return;
    if (!document.fullscreenElement) {
      void container.requestFullscreen().catch(() => undefined);
    } else {
      void document.exitFullscreen().catch(() => undefined);
    }
  }, [playerContainerRef]);

  const togglePictureInPicture = useCallback(() => {
    const video = videoRef.current;
    if (!video) return;
    if (document.pictureInPictureElement) {
      void document.exitPictureInPicture().catch(() => undefined);
    } else if (document.pictureInPictureEnabled) {
      void video.requestPictureInPicture().catch(() => undefined);
    }
  }, [videoRef]);

  const changePlaybackRate = useCallback((rate: number) => {
    const video = videoRef.current;
    if (!video) return;
    video.playbackRate = rate;
    setPlaybackRate(rate);
  }, [videoRef]);

  return {
    volume,
    isMuted,
    playbackRate,
    isFullscreen,
    centerSplash,
    togglePlayPause,
    seekTo,
    seekRelative,
    toggleMute,
    changeVolume,
    changePlaybackRate,
    toggleFullscreen,
    togglePictureInPicture,
  };
}
