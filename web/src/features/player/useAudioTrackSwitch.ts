import { useCallback, useMemo } from "react";
import type { MediaSource, WebPlaybackPlan } from "../../lib/api/types";
import {
  audioParamFromQueryKey,
  audioTrackOptions,
  audioTrackSwitchParams,
  resolveConfirmedAudioTrackIndex,
  supportsAudioTrackSwitching,
  type AudioTrackOwner,
} from "./audio-track-selection";
import { usePlaybackVariantSwitch } from "./usePlaybackVariantSwitch";
import type { PlaybackReporting } from "./usePlaybackReporting";

type AudioTrackSwitchOptions = {
  itemId: string;
  playbackKey: string;
  source: MediaSource | undefined;
  requestedAudioStreamIndex: string | null;
  /** The plan of the current session: it decides who owns the audible track. */
  playbackPlan: WebPlaybackPlan | undefined;
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
  playbackPlan,
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
  // A server-side session fixes the audio stream it muxes, so it confirms the
  // explicit index or the first audio stream. Direct Play leaves the choice to the
  // browser container, so the URL selection is the only proof there.
  const audioTrackOwner: AudioTrackOwner =
    playbackPlan?.type === "SERVER_HLS" ? "server" : "browser";
  const selectedAudioStreamIndex = resolveConfirmedAudioTrackIndex(
    source,
    explicitAudioStreamIndex,
    audioTrackOwner,
  );
  // Direct Play cannot select a non-first STRM audio stream, so those sources
  // never expose the track switcher. Only a confirmed index is marked "当前".
  const audioTracks = useMemo(
    () => supportsAudioTrackSwitching(source)
      ? audioTrackOptions(source, selectedAudioStreamIndex)
      : [],
    [selectedAudioStreamIndex, source],
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
    // Only the explicit URL selection can make this a no-op. Without one the
    // audible track is unproven, so choosing the first audio stream must still
    // rebuild the session: Direct Play needs the explicit index to pin it.
    if (switching || !targetIndex || targetIndex === explicitAudioStreamIndex) return;
    const isAudioTrack = (source.streams ?? []).some(
      (stream) => (stream.type ?? "").toUpperCase() === "AUDIO" && String(stream.index) === targetIndex,
    );
    if (!isAudioTrack) return;
    beginSwitch(audioTrackSwitchParams(searchParams, targetIndex, source, audioTrackOwner));
  }, [audioTrackOwner, beginSwitch, explicitAudioStreamIndex, searchParams, source, switching]);

  return {
    audioTracks,
    selectedAudioStreamIndex,
    switching,
    notice,
    switchAudioTrack,
  };
}
