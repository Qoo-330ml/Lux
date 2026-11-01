import { useCallback, useMemo } from "react";
import type { MediaSource, WebPlaybackBitrate } from "../../lib/api/types";
import {
  bitrateParamFromQueryKey,
  canonicalMaxBitrateParam,
  parseMaxBitrateParam,
  qualityOptions,
  qualityStatusText,
  qualitySwitchParams,
  supportsQualitySelection,
  type QualityOption,
} from "./playback-quality";
import { usePlaybackVariantSwitch } from "./usePlaybackVariantSwitch";
import type { PlaybackReporting } from "./usePlaybackReporting";

type QualitySwitchOptions = {
  itemId: string;
  playbackKey: string;
  source: MediaSource | undefined;
  /** Raw `maxBitrate` URL value; the URL is the single source of truth. */
  requestedMaxBitrate: string | null;
  /** `bitrate` block of the current session; drives the effective-status line. */
  playbackBitrate: WebPlaybackBitrate | undefined;
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

export type QualitySwitch = {
  options: QualityOption[];
  /** Value of the currently selected option ("" is the automatic choice). */
  selectedValue: string;
  /** What the session is actually doing, from the response `bitrate` block. */
  status: string;
  switching: boolean;
  notice: string | null;
  switchQuality: (value: string) => void;
};

const QUALITY_SWITCH_MESSAGES = {
  timeout: "画质切换超时，已恢复原画。",
  failure: "画质切换失败，已恢复原画。",
  autoplayBlocked: "已切换画质，但浏览器阻止了自动播放，请手动继续。",
};

/**
 * Owns the manual quality (bitrate ceiling) switch. Picking a ceiling rewrites
 * the `maxBitrate` URL parameter and rebuilds the session at the current
 * position through `usePlaybackVariantSwitch`, the same machinery the audio
 * track switch uses, so an active audio selection is carried along unchanged.
 *
 * This is a single-shot ceiling, not adaptive bitrate: the server decides once
 * per session and the response's `bitrate` block describes the result.
 */
export function useQualitySwitch({
  itemId,
  playbackKey,
  source,
  requestedMaxBitrate,
  playbackBitrate,
  searchParams,
  setSearchParams,
  streamUrl,
  playing,
  playbackRate,
  sessionFailed,
  videoRef,
  reportPlayback,
  resetControlsTimeout,
}: QualitySwitchOptions): QualitySwitch {
  const selectedMaxBitrate = parseMaxBitrateParam(requestedMaxBitrate);
  const options = useMemo(
    () => qualityOptions(source, selectedMaxBitrate),
    [selectedMaxBitrate, source],
  );
  const status = qualityStatusText(playbackBitrate, selectedMaxBitrate);
  const { switching, notice, beginSwitch } = usePlaybackVariantSwitch({
    itemId,
    playbackKey,
    param: "maxBitrate",
    // Query keys store the parsed bps, so every cache predicate must compare
    // against the same canonical text instead of the raw URL value.
    requestedValue: selectedMaxBitrate === null ? null : String(selectedMaxBitrate),
    canonicalValue: canonicalMaxBitrateParam,
    queryVariant: bitrateParamFromQueryKey,
    searchParams,
    setSearchParams,
    streamUrl,
    playing,
    playbackRate,
    sessionFailed,
    videoRef,
    reportPlayback,
    resetControlsTimeout,
    messages: QUALITY_SWITCH_MESSAGES,
  });

  const switchQuality = useCallback((value: string) => {
    if (!supportsQualitySelection(source) || switching) return;
    const option = options.find((entry) => entry.value === value);
    // Unknown values (hand-edited URL or a tier this source does not offer) and
    // the current choice are ignored instead of rebuilding the session.
    if (!option || option.isCurrent) return;
    beginSwitch(qualitySwitchParams(searchParams, option.bitrate));
  }, [beginSwitch, options, searchParams, source, switching]);

  return {
    options,
    selectedValue: options.find((option) => option.isCurrent)?.value ?? "",
    status,
    switching,
    notice,
    switchQuality,
  };
}
