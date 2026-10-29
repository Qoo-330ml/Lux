import { useCallback, useEffect, useRef, useState } from "react";
import type { MediaSource } from "../../lib/api/types";
import { normalizeCaptionOffset } from "./caption-offset";
import type { LuxCaptionCue } from "./caption-parser";
import {
  defaultCaptionSelection,
  nativeCaptionTrack,
  overlayCaptionSource,
  playerCaptionOptions,
  type PlayerCaptionOption,
  type PlayerNativeCaptionTrack,
  type PlayerOverlayCaptionSource,
  type PlayerRuntimeCaptionTrack,
} from "./components/player-captions";
import type { Dispatch, SetStateAction } from "react";

export type RuntimeCaptionCueInput = {
  trackId: string;
  startMs: number;
  endMs: number;
  text: string;
  layer?: number;
  alignment?: number;
  position?: { x: number; y: number };
  style?: { color?: string; bold?: boolean; italic?: boolean; marginL?: number; marginR?: number; marginV?: number };
  runs?: readonly { text: string; color?: string; bold?: boolean; italic?: boolean }[];
};

export type CaptionSelection = {
  captionOptions: PlayerCaptionOption[];
  selectedCaptionOption: PlayerCaptionOption | null;
  selectedCaptionId: string | null;
  captionSourceId: string | null;
  captionStatus: string | null;
  captionOffset: number;
  runtimeCaptionOverlayCues: LuxCaptionCue[];
  captionTrack: PlayerNativeCaptionTrack | null;
  captionOverlaySource: PlayerOverlayCaptionSource | null;
  nativeCaptionTrackId: string | null;
  selectCaption: (id: string | null) => void;
  handleRuntimeCaptionCue: (cue: RuntimeCaptionCueInput) => void;
  changeCaptionOffset: (offset: number) => void;
  clearRuntimeCaptionCues: () => void;
  setCaptionStatus: (status: string | null) => void;
  setNativeCaptionTracks: Dispatch<SetStateAction<PlayerRuntimeCaptionTrack[]>>;
};

type CaptionSelectionOptions = {
  itemId: string;
  source: MediaSource | undefined;
  nativeCaptionTracksSupported: boolean;
  requestedSubtitleStreamIndex: string | null;
  resetControlsTimeout: () => void;
};

/**
 * Owns per-source caption options, the selected track, caption status/offset and
 * the runtime overlay cue buffer. Selection resets per source but a manual choice
 * survives later default recomputations for the same source.
 */
export function useCaptionSelection({
  itemId,
  source,
  nativeCaptionTracksSupported,
  requestedSubtitleStreamIndex,
  resetControlsTimeout,
}: CaptionSelectionOptions): CaptionSelection {
  const [selectedCaptionId, setSelectedCaptionId] = useState<string | null>(null);
  const [captionSourceId, setCaptionSourceId] = useState<string | null>(null);
  const [captionStatus, setCaptionStatus] = useState<string | null>(null);
  const [captionOffset, setCaptionOffset] = useState(0);
  const [runtimeCaptionCues, setRuntimeCaptionCues] = useState<LuxCaptionCue[]>([]);
  const [nativeCaptionTracks, setNativeCaptionTracks] = useState<PlayerRuntimeCaptionTrack[]>([]);
  const captionSelectionTouchedRef = useRef(false);

  const captionOptions = playerCaptionOptions(source, nativeCaptionTracksSupported, nativeCaptionTracks);
  const selectedCaptionOption = captionSourceId === source?.id
    ? captionOptions.find((caption) => (
      (caption.id === selectedCaptionId || String(caption.streamIndex) === selectedCaptionId)
      && caption.available
    )) ?? null
    : null;
  const captionTrack = nativeCaptionTrack(itemId, source?.id ?? "", selectedCaptionOption);
  const captionOverlaySource = overlayCaptionSource(itemId, source?.id ?? "", selectedCaptionOption);
  const runtimeCaptionOverlayCues = selectedCaptionOption?.renderMode === "runtime-overlay"
    ? runtimeCaptionCues.filter((cue) => cue.id.startsWith(`${selectedCaptionOption.id}:`))
    : [];
  const nativeCaptionTrackId = selectedCaptionOption?.renderMode === "native-inband"
    ? selectedCaptionOption.runtimeTrackId ?? null
    : null;

  useEffect(() => {
    captionSelectionTouchedRef.current = false;
    setNativeCaptionTracks([]);
    const initialCaptionOptions = playerCaptionOptions(source, nativeCaptionTracksSupported, []);
    const initialCaption = (requestedSubtitleStreamIndex
      ? initialCaptionOptions.find((caption) => (
        String(caption.streamIndex) === requestedSubtitleStreamIndex && caption.available
      ))
      : undefined) ?? defaultCaptionSelection(initialCaptionOptions);
    setCaptionSourceId(source?.id ?? null);
    setSelectedCaptionId(initialCaption?.id ?? null);
    setCaptionStatus(null);
    setRuntimeCaptionCues([]);
  }, [nativeCaptionTracksSupported, requestedSubtitleStreamIndex, source?.id]);

  const defaultCaptionId = defaultCaptionSelection(captionOptions)?.id ?? null;
  useEffect(() => {
    if (
      captionSelectionTouchedRef.current
      || !source?.id
      || captionSourceId !== source.id
    ) {
      return;
    }
    setSelectedCaptionId((previous) => (
      previous === defaultCaptionId ? previous : defaultCaptionId
    ));
  }, [captionSourceId, defaultCaptionId, source?.id]);

  const selectCaption = useCallback((id: string | null) => {
    captionSelectionTouchedRef.current = true;
    if (id === null) {
      setCaptionSourceId(source?.id ?? null);
      setSelectedCaptionId(null);
      setCaptionStatus(null);
      resetControlsTimeout();
      return;
    }
    const option = captionOptions.find((caption) => caption.id === id);
    if (!option?.available) return;
    setCaptionSourceId(source?.id ?? null);
    setSelectedCaptionId(option.id);
    setCaptionStatus(option.renderMode === "native-inband" ? null : "字幕加载中…");
    resetControlsTimeout();
  }, [captionOptions, resetControlsTimeout, source]);

  const handleRuntimeCaptionCue = useCallback((cue: RuntimeCaptionCueInput) => {
    setCaptionStatus(null);
    const next: LuxCaptionCue = {
      id: `${cue.trackId}:${cue.startMs}:${cue.endMs}:${cue.layer ?? 0}:${cue.text.slice(0, 16)}`,
      start: cue.startMs / 1000,
      end: cue.endMs / 1000,
      text: cue.text,
      layer: cue.layer,
      alignment: cue.alignment,
      position: cue.position,
      style: cue.style,
      runs: cue.runs,
    };
    setRuntimeCaptionCues((previous) => previous.some((entry) => entry.id === next.id) ? previous : [...previous, next]);
  }, []);

  const changeCaptionOffset = useCallback((offset: number) => {
    setCaptionOffset(normalizeCaptionOffset(offset));
    resetControlsTimeout();
  }, [resetControlsTimeout]);

  const clearRuntimeCaptionCues = useCallback(() => {
    setRuntimeCaptionCues([]);
  }, []);

  return {
    captionOptions,
    selectedCaptionOption,
    selectedCaptionId,
    captionSourceId,
    captionStatus,
    captionOffset,
    runtimeCaptionOverlayCues,
    captionTrack,
    captionOverlaySource,
    nativeCaptionTrackId,
    selectCaption,
    handleRuntimeCaptionCue,
    changeCaptionOffset,
    clearRuntimeCaptionCues,
    setCaptionStatus,
    setNativeCaptionTracks,
  };
}
