import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import type { MediaSource } from "../../lib/api/types";
import {
  audioParamFromQueryKey,
  audioTrackOptions,
  audioTrackSwitchParams,
  resolveSelectedAudioTrackIndex,
  supportsAudioTrackSwitching,
} from "./audio-track-selection";
import type { PlaybackReporting } from "./usePlaybackReporting";

const RESUME_TIMEOUT_MS = 15_000;

type PendingResume = {
  /** URL value that should appear once the new session has been requested. */
  targetParam: string | null;
  position: number;
  playbackRate: number;
  volume: number;
  muted: boolean;
  playing: boolean;
  /** URL value to restore when the new session cannot be created. */
  previousParam: string | null;
};

type AudioTrackSwitchOptions = {
  itemId: string;
  source: MediaSource | undefined;
  requestedAudioStreamIndex: string | null;
  searchParams: URLSearchParams;
  setSearchParams: (next: URLSearchParams) => void;
  streamUrl: string;
  playing: boolean;
  playbackRate: number;
  sessionFailed: boolean;
  videoRef: { current: HTMLVideoElement | null };
  reportPlayback: PlaybackReporting["reportPlayback"];
  resetControlsTimeout: () => void;
};

export type AudioTrackSwitch = {
  audioTracks: ReturnType<typeof audioTrackOptions>;
  selectedAudioStreamIndex: string | null;
  switching: boolean;
  notice: string | null;
  switchAudioTrack: (index: string) => void;
};

function normalizeParam(value: string | null | undefined): string | null {
  return value?.trim() || null;
}

/**
 * Owns the in-player audio track switch. Switching rebuilds the playback
 * session through the existing `audioStreamIndex` URL parameter, so the URL
 * stays refresh-safe and the session gate keeps handling old-session shutdown.
 * The hook persists the live position/state first, then restores position,
 * speed and pause/play state on the replacement media element.
 */
export function useAudioTrackSwitch({
  itemId,
  source,
  requestedAudioStreamIndex,
  searchParams,
  setSearchParams,
  streamUrl,
  playing,
  playbackRate,
  sessionFailed,
  videoRef,
  reportPlayback,
  resetControlsTimeout,
}: AudioTrackSwitchOptions): AudioTrackSwitch {
  const queryClient = useQueryClient();
  const [switching, setSwitching] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const pendingRef = useRef<PendingResume | null>(null);
  const revertSwitch = useCallback((pending: PendingResume, message: string) => {
    setNotice(message);
    const reverted = new URLSearchParams(searchParams);
    if (pending.previousParam) reverted.set("audioStreamIndex", pending.previousParam);
    else reverted.delete("audioStreamIndex");
    setSearchParams(reverted);
    void queryClient.resetQueries({ queryKey: ["playback-bootstrap"] });
    void queryClient.resetQueries({ queryKey: ["web-playback-session"] });
  }, [queryClient, searchParams, setSearchParams]);
  const explicitAudioStreamIndex = normalizeParam(requestedAudioStreamIndex);
  const selectedAudioStreamIndex = resolveSelectedAudioTrackIndex(source, explicitAudioStreamIndex);
  // Direct Play cannot select a non-first STRM audio stream, so those sources
  // never expose the track switcher. Without an explicit URL selection the
  // browser may pick any Direct Play track, so no option is marked "current".
  const audioTracks = useMemo(
    () => supportsAudioTrackSwitching(source)
      ? audioTrackOptions(source, explicitAudioStreamIndex)
      : [],
    [explicitAudioStreamIndex, source],
  );

  const switchAudioTrack = useCallback((index: string) => {
    const targetIndex = index.trim();
    if (!source || !supportsAudioTrackSwitching(source)) return;
    if (switching || !targetIndex || targetIndex === selectedAudioStreamIndex) return;
    const isAudioTrack = (source.streams ?? []).some(
      (stream) => (stream.type ?? "").toUpperCase() === "AUDIO" && String(stream.index) === targetIndex,
    );
    if (!isAudioTrack) return;
    // Cached bootstrap/session entries for other audio variants reference
    // sessions that were stopped when this player left them. Dropping them
    // forces a fresh session when the user later switches back.
    const currentAudioKey = explicitAudioStreamIndex ?? "default";
    queryClient.removeQueries({
      predicate: (query) => {
        const [prefix, queryItemId] = query.queryKey;
        if (queryItemId !== itemId) return false;
        if (prefix !== "playback-bootstrap" && prefix !== "web-playback-session") return false;
        return audioParamFromQueryKey(query.queryKey) !== currentAudioKey;
      },
    });
    const video = videoRef.current;
    const position = video && Number.isFinite(video.currentTime) ? Math.max(0, video.currentTime) : 0;
    const volume = video && Number.isFinite(video.volume) ? Math.max(0, Math.min(1, video.volume)) : 1;
    const muted = video ? video.muted : false;
    const nextParams = audioTrackSwitchParams(searchParams, targetIndex, source);
    pendingRef.current = {
      targetParam: normalizeParam(nextParams.get("audioStreamIndex")),
      position,
      playbackRate,
      volume,
      muted,
      playing,
      previousParam: normalizeParam(searchParams.get("audioStreamIndex")),
    };
    setNotice(null);
    setSwitching(true);
    resetControlsTimeout();
    // Persist the live position before the bootstrap request reads it back, so
    // a refresh and the fallback resume path agree on where playback was.
    const report = reportPlayback(playing ? "PLAYING" : "PAUSED", true);
    const apply = () => setSearchParams(nextParams);
    if (report && typeof (report as Promise<unknown>).finally === "function") {
      void (report as Promise<unknown>).catch(() => undefined).finally(apply);
    } else {
      apply();
    }
  }, [
    itemId,
    explicitAudioStreamIndex,
    playbackRate,
    playing,
    queryClient,
    reportPlayback,
    resetControlsTimeout,
    searchParams,
    selectedAudioStreamIndex,
    setSearchParams,
    source,
    switching,
    videoRef,
  ]);

  useEffect(() => {
    const pending = pendingRef.current;
    if (!pending || sessionFailed) return undefined;
    if (normalizeParam(requestedAudioStreamIndex) !== pending.targetParam) return undefined;
    if (!streamUrl) return undefined;
    const video = videoRef.current;
    if (!video) return undefined;

    let cancelled = false;
    let applied = false;
    const apply = () => {
      if (cancelled || applied) return;
      applied = true;
      if (Number.isFinite(pending.position) && pending.position > 0) {
        const maximum = Number.isFinite(video.duration) && video.duration > 0
          ? Math.max(0, video.duration - 0.25)
          : pending.position;
        const target = Math.min(pending.position, maximum);
        if (Math.abs(video.currentTime - target) > 0.25) video.currentTime = target;
      }
      video.playbackRate = pending.playbackRate;
      video.volume = pending.volume;
      video.muted = pending.muted;
      if (pending.playing) {
        void video.play().catch(() => {
          if (!cancelled) setNotice("已切换音轨，但浏览器阻止了自动播放，请手动继续。");
        });
      } else {
        video.pause();
      }
      pendingRef.current = null;
      setSwitching(false);
    };
    const applyWhenReady = () => {
      // `loadedmetadata` already ran the runtime's server-position restore;
      // wait for media data so this correction is applied afterwards.
      if (video.readyState >= 3) apply();
    };

    video.addEventListener("canplay", apply);
    video.addEventListener("loadeddata", apply);
    applyWhenReady();

    return () => {
      cancelled = true;
      video.removeEventListener("canplay", apply);
      video.removeEventListener("loadeddata", apply);
    };
  }, [requestedAudioStreamIndex, sessionFailed, streamUrl, videoRef]);

  useEffect(() => {
    if (!switching) return undefined;
    // The replacement plan can be UNSUPPORTED and never produce a stream URL,
    // so the timeout must not depend on `streamUrl` changing.
    const timeout = window.setTimeout(() => {
      const pending = pendingRef.current;
      if (!pending) return;
      pendingRef.current = null;
      setSwitching(false);
      revertSwitch(pending, "音轨切换超时，已恢复原音轨。");
    }, RESUME_TIMEOUT_MS);
    return () => window.clearTimeout(timeout);
  }, [revertSwitch, switching]);

  useEffect(() => {
    const pending = pendingRef.current;
    if (!pending || !sessionFailed) return;
    pendingRef.current = null;
    setSwitching(false);
    revertSwitch(pending, "音轨切换失败，已恢复原音轨。");
  }, [revertSwitch, sessionFailed]);

  return {
    audioTracks,
    selectedAudioStreamIndex,
    switching,
    notice,
    switchAudioTrack,
  };
}
