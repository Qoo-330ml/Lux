// @vitest-environment jsdom

import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { MediaSource } from "../src/lib/api/types";
import {
  useCaptionSelection,
  type CaptionSelection,
} from "../src/features/player/useCaptionSelection";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let container: HTMLDivElement | undefined;
let root: Root | undefined;

afterEach(() => {
  if (root) act(() => root?.unmount());
  root = undefined;
  container?.remove();
  container = undefined;
  vi.restoreAllMocks();
});

const VTT_SOURCE: MediaSource = {
  id: "source-vtt",
  isDefault: true,
  streams: [
    { index: 0, type: "VIDEO", codec: "h264" },
    { index: 2, type: "SUBTITLE", codec: "vtt", language: "zho", isExternal: true, isDefault: true },
    { index: 3, type: "SUBTITLE", codec: "srt", language: "eng", isExternal: true },
  ],
};

const OTHER_SOURCE: MediaSource = {
  id: "source-other",
  streams: [
    { index: 5, type: "SUBTITLE", codec: "vtt", language: "jpn", isExternal: true, isDefault: true },
  ],
};

const REMOTE_MKV_SOURCE: MediaSource = {
  id: "source-remote",
  sourceKind: "STRM_URL",
  container: "mkv",
  externalUrl: "https://media.example.test/media.mkv",
  streams: [
    { index: 1, type: "SUBTITLE", codec: "srt", language: "zho", isExternal: false, isDefault: true },
  ],
};

type HarnessProps = {
  source: MediaSource | undefined;
  requestedSubtitleStreamIndex?: string | null;
  nativeCaptionTracksSupported?: boolean;
};

function mountHarness(initial: HarnessProps) {
  const state: {
    selection?: CaptionSelection;
    resetControlsTimeout: ReturnType<typeof vi.fn>;
    rerender: (next: HarnessProps) => void;
  } = { resetControlsTimeout: vi.fn(), rerender: () => undefined };

  function Harness() {
    const [props, setProps] = useState<HarnessProps>(initial);
    state.rerender = (next: HarnessProps) => setProps(next);
    state.selection = useCaptionSelection({
      itemId: "movie-1",
      source: props.source,
      nativeCaptionTracksSupported: props.nativeCaptionTracksSupported ?? false,
      requestedSubtitleStreamIndex: props.requestedSubtitleStreamIndex ?? null,
      resetControlsTimeout: state.resetControlsTimeout,
      engineRef: { current: null },
    });
    return null;
  }

  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  act(() => root?.render(<Harness />));
  return state;
}

describe("useCaptionSelection", () => {
  it("starts on the default selectable caption of the current source", () => {
    const harness = mountHarness({ source: VTT_SOURCE, nativeCaptionTracksSupported: true });
    const selection = harness.selection;

    expect(selection?.captionSourceId).toBe("source-vtt");
    expect(selection?.selectedCaptionId).toBe("2");
    expect(selection?.captionOptions.map((option) => option.id)).toEqual(["2", "3"]);
    expect(selection?.captionTrack?.src).toBe("/api/v1/items/movie-1/subtitles/2?sourceId=source-vtt");
    expect(selection?.captionOverlaySource).toBeNull();
    expect(selection?.captionStatus).toBeNull();
    expect(selection?.captionOffset).toBe(0);
  });

  it("preserves the pre-refactor rule that an untouched URL index settles on the source default", () => {
    // The URL index seeds the first selection, then the "follow the default while
    // untouched" effect re-applies the source default on the next effect flush.
    // This is the current player behavior and must not change during refactors.
    const harness = mountHarness({ source: VTT_SOURCE, requestedSubtitleStreamIndex: "3" });
    expect(harness.selection?.selectedCaptionId).toBe("2");

    act(() => harness.selection?.selectCaption("3"));
    expect(harness.selection?.selectedCaptionId).toBe("3");
  });

  it("keeps a manual choice when defaults are recomputed", () => {
    const harness = mountHarness({ source: VTT_SOURCE });
    act(() => harness.selection?.selectCaption(null));
    expect(harness.selection?.selectedCaptionId).toBeNull();
    expect(harness.resetControlsTimeout).toHaveBeenCalledTimes(1);

    act(() => harness.rerender({ source: VTT_SOURCE }));
    expect(harness.selection?.selectedCaptionId).toBeNull();

    act(() => harness.selection?.selectCaption("3"));
    expect(harness.selection?.selectedCaptionId).toBe("3");
    act(() => harness.rerender({ source: VTT_SOURCE }));
    expect(harness.selection?.selectedCaptionId).toBe("3");
  });

  it("follows the new default while the user has not touched the selection", () => {
    const harness = mountHarness({ source: VTT_SOURCE });
    expect(harness.selection?.selectedCaptionId).toBe("2");

    act(() => harness.rerender({ source: OTHER_SOURCE }));
    expect(harness.selection?.captionSourceId).toBe("source-other");
    expect(harness.selection?.selectedCaptionId).toBe("5");
  });

  it("ignores unavailable captions and reports a loading status for overlays", () => {
    const source: MediaSource = {
      ...VTT_SOURCE,
      streams: [
        { index: 2, type: "SUBTITLE", codec: "ass", isExternal: true, isDefault: true },
        { index: 3, type: "SUBTITLE", codec: "vtt", isExternal: true },
        { index: 4, type: "SUBTITLE", codec: "vtt", isExternal: false },
      ],
    };
    const harness = mountHarness({ source, nativeCaptionTracksSupported: true });
    const optionIds = harness.selection?.captionOptions.map((option) => `${option.id}:${option.renderMode}`) ?? [];
    expect(optionIds).toEqual(["2:overlay", "3:native", "4:overlay"]);

    const unavailable = harness.selection?.captionOptions.find((option) => option.id === "2");
    expect(unavailable?.available).toBe(true);

    act(() => harness.selection?.selectCaption("3"));
    expect(harness.selection?.captionStatus).toBe("字幕加载中…");

    act(() => harness.selection?.selectCaption("2"));
    expect(harness.selection?.captionStatus).toBe("字幕加载中…");

    act(() => harness.selection?.selectCaption("missing"));
    expect(harness.selection?.selectedCaptionId).toBe("2");
  });

  it("clears the status for in-band caption tracks", () => {
    const source: MediaSource = {
      ...VTT_SOURCE,
      streams: [{ index: 4, type: "SUBTITLE", codec: "vtt", isExternal: false, isDefault: true }],
    };
    const harness = mountHarness({ source });
    const option = harness.selection?.captionOptions[0];
    expect(option?.renderMode).toBe("overlay");

    act(() => harness.selection?.setNativeCaptionTracks([
      { id: "inband-caption-0", label: "轨道 1", kind: "subtitles", ordinal: 0 },
    ]));
    const inband = harness.selection?.captionOptions[0];
    expect(inband?.renderMode).toBe("native-inband");
    expect(harness.selection?.nativeCaptionTrackId).toBe("inband-caption-0");

    act(() => harness.selection?.selectCaption(inband?.id ?? ""));
    expect(harness.selection?.captionStatus).toBeNull();
  });

  it("normalizes the caption offset", () => {
    const harness = mountHarness({ source: VTT_SOURCE });
    act(() => harness.selection?.changeCaptionOffset(99));
    expect(harness.selection?.captionOffset).toBe(10);
    act(() => harness.selection?.changeCaptionOffset(0.26));
    expect(harness.selection?.captionOffset).toBe(0.3);
    act(() => harness.selection?.changeCaptionOffset(Number.NaN));
    expect(harness.selection?.captionOffset).toBe(0);
    expect(harness.resetControlsTimeout).toHaveBeenCalledTimes(3);
  });

  it("de-duplicates runtime overlay cues and only exposes cues of the selected track", () => {
    const harness = mountHarness({ source: REMOTE_MKV_SOURCE });
    act(() => harness.selection?.selectCaption("1"));
    expect(harness.selection?.selectedCaptionOption?.renderMode).toBe("runtime-overlay");

    const cue = { trackId: "1", startMs: 1_000, endMs: 2_000, text: "你好" };
    act(() => harness.selection?.handleRuntimeCaptionCue(cue));
    act(() => harness.selection?.handleRuntimeCaptionCue(cue));
    expect(harness.selection?.runtimeCaptionOverlayCues).toHaveLength(1);
    expect(harness.selection?.captionStatus).toBeNull();

    act(() => harness.selection?.clearRuntimeCaptionCues());
    expect(harness.selection?.runtimeCaptionOverlayCues).toHaveLength(0);
  });
});
