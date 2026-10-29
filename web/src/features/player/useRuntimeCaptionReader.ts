import { useEffect, useRef } from "react";
import type { PlayerCaptionOption } from "./components/player-captions";
import { RemoteMkvCaptionReader } from "./remote-mkv-caption-reader";
import type { RuntimeCaptionCueInput } from "./useCaptionSelection";

type VideoRef = { current: HTMLVideoElement | null };
type CurrentTimeRef = { current: number };

type RuntimeCaptionReaderOptions = {
  clientEngineActive: boolean;
  clientMkvSourceUrl: string | null;
  remoteCaptionSidecarRequested: boolean;
  playbackKey: string;
  currentTime: number;
  selectedCaptionOption: PlayerCaptionOption | null;
  videoRef: VideoRef;
  currentTimeRef: CurrentTimeRef;
  onCue: (cue: RuntimeCaptionCueInput) => void;
  onStatusChange: (status: string | null) => void;
  clearCues: () => void;
};

/**
 * Streams cues from a remote Matroska sidecar reader while a runtime-overlay
 * caption is selected on a direct remote source. The reader is torn down for
 * every other selection and whenever the playback generation changes.
 */
export function useRuntimeCaptionReader({
  clientEngineActive,
  clientMkvSourceUrl,
  remoteCaptionSidecarRequested,
  playbackKey,
  currentTime,
  selectedCaptionOption,
  videoRef,
  currentTimeRef,
  onCue,
  onStatusChange,
  clearCues,
}: RuntimeCaptionReaderOptions) {
  const readerRef = useRef<RemoteMkvCaptionReader | null>(null);

  useEffect(() => {
    clearCues();
    if (
      !remoteCaptionSidecarRequested
      || !clientMkvSourceUrl
      || !selectedCaptionOption
      || selectedCaptionOption.renderMode !== "runtime-overlay"
    ) {
      readerRef.current?.destroy();
      readerRef.current = null;
      return undefined;
    }
    let active = true;
    const selection = selectedCaptionOption;
    const reader = new RemoteMkvCaptionReader({
      source: clientMkvSourceUrl,
      selection: {
        id: selection.id,
        name: selection.name,
        language: selection.language,
        format: selection.format,
        ordinal: selection.embeddedOrdinal,
      },
      currentTime: () => videoRef.current?.currentTime ?? currentTimeRef.current,
      onReady: () => {
        if (active) onStatusChange(null);
      },
      onCue: (cue) => {
        if (!active) return;
        onCue({
          trackId: selection.id,
          startMs: cue.start * 1000,
          endMs: cue.end * 1000,
          text: cue.text,
          layer: cue.layer,
          alignment: cue.alignment,
          position: cue.position,
          style: cue.style,
          runs: cue.runs,
        });
      },
      onError: () => {
        if (active) onStatusChange("远程字幕不可用：上游不支持 CORS/Range 或媒体索引不可用");
      },
    });
    readerRef.current = reader;
    void reader.start();
    return () => {
      active = false;
      reader.destroy();
      if (readerRef.current === reader) readerRef.current = null;
      clearCues();
    };
  }, [
    clientEngineActive,
    clientMkvSourceUrl,
    clearCues,
    currentTimeRef,
    onCue,
    onStatusChange,
    playbackKey,
    remoteCaptionSidecarRequested,
    selectedCaptionOption?.embeddedOrdinal,
    selectedCaptionOption?.format,
    selectedCaptionOption?.id,
    selectedCaptionOption?.language,
    selectedCaptionOption?.name,
    selectedCaptionOption?.renderMode,
    videoRef,
  ]);

  useEffect(() => {
    readerRef.current?.setTime(currentTime);
  }, [currentTime]);
}
