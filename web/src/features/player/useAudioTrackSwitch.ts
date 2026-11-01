import { useCallback, useMemo } from "react";
import type { MediaSource } from "../../lib/api/types";
import {
  audioParamFromQueryKey,
  audioTrackOptions,
  audioTrackSwitchParams,
  resolveSelectedAudioTrackIndex,
  supportsAudioTrackSwitching,
} from "./audio-track-selection";
import { usePlaybackVariantSwitch } from "./usePlaybackVariantSwitch";
import type { PlaybackReporting } from "./usePlaybackReporting";

type AudioTrackSwitchOptions = {
  itemId: string;
  playbackKey: string;
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

const AUDIO_SWITCH_MESSAGES = {
  timeout: "音轨切换超时，已恢复原音轨。",
  failure: "音轨切换失败，已恢复原音轨。",
  autoplayBlocked: "已切换音轨，但浏览器阻止了自动播放，请手动继续。",
};

/**
 * Owns the in-player audio track switch. Switching rebuilds the playback
 * session through the existing `audioStreamIndex` URL parameter, so the URL
 * stays refresh-safe and the session gate keeps handling old-session shutdown.
 * The shared choreography (persist the live position/state, restore position,
 * speed and pause/play state on the replacement media, roll back on failure)
 * lives in `usePlaybackVariantSwitch` and is shared with the quality switch.
 */
export function useAudioTrackSwitch({
  itemId,
  playbackKey,
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
  const explicitAudioStreamIndex = requestedAudioStreamIndex?.trim() || null;
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

  const { switching, notice, beginSwitch } = usePlaybackVariantSwitch({
    itemId,
    playbackKey,
    param: "audioStreamIndex",
    requestedValue: requestedAudioStreamIndex,
    queryVariant: audioParamFromQueryKey,
    searchParams,
    setSearchParams,
    streamUrl,
    playing,
    playbackRate,
    sessionFailed,
    videoRef,
    reportPlayback,
    resetControlsTimeout,
    messages: AUDIO_SWITCH_MESSAGES,
  });

  const switchAudioTrack = useCallback((index: string) => {
    const targetIndex = index.trim();
    if (!source || !supportsAudioTrackSwitching(source)) return;
    if (switching || !targetIndex || targetIndex === selectedAudioStreamIndex) return;
    const isAudioTrack = (source.streams ?? []).some(
      (stream) => (stream.type ?? "").toUpperCase() === "AUDIO" && String(stream.index) === targetIndex,
    );
    if (!isAudioTrack) return;
    beginSwitch(audioTrackSwitchParams(searchParams, targetIndex, source));
  }, [beginSwitch, searchParams, selectedAudioStreamIndex, source, switching]);

  return {
    audioTracks,
    selectedAudioStreamIndex,
    switching,
    notice,
    switchAudioTrack,
  };
}
