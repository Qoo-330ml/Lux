// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it } from "vitest";
import { PlayerVideoSurface } from "../src/features/player/components/player-video-surface";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("LuxPlayer native caption lifecycle", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;
  let trackDescriptor: PropertyDescriptor | undefined;

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
    if (trackDescriptor) Object.defineProperty(HTMLTrackElement.prototype, "track", trackDescriptor);
    else delete (HTMLTrackElement.prototype as HTMLTrackElement & { track?: TextTrack }).track;
    trackDescriptor = undefined;
  });

  it("shifts the current track and restores its cues when the surface is destroyed", async () => {
    const first = { startTime: 1, endTime: 3 };
    const cues: Array<typeof first> = [first];
    const nativeTrack = {
      mode: "disabled" as TextTrackMode,
      cues,
      addCue(cue: typeof first) {
        if (!cues.includes(cue)) cues.push(cue);
      },
      removeCue(cue: typeof first) {
        const index = cues.indexOf(cue);
        if (index >= 0) cues.splice(index, 1);
      },
    } as unknown as TextTrack;
    trackDescriptor = Object.getOwnPropertyDescriptor(HTMLTrackElement.prototype, "track");
    Object.defineProperty(HTMLTrackElement.prototype, "track", {
      configurable: true,
      get: () => nativeTrack,
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    await act(async () => {
      root?.render(
        <PlayerVideoSurface
          streamUrl="/video"
          title="字幕生命周期"
          videoRef={() => undefined}
          onClick={() => undefined}
          onDoubleClick={() => undefined}
          captionTrack={{ id: "caption-1", label: "中文", src: "/caption.vtt" }}
          captionOffset={1}
          captionDuration={10}
          centerSplash={null}
          fallbackLoading={false}
          fallbackSpeedX={null}
          errorMessage={null}
          showError={false}
          onRetry={() => undefined}
          onBack={() => undefined}
        />,
      );
    });
    expect(first).toMatchObject({ startTime: 2, endTime: 4 });
    expect(nativeTrack.mode).toBe("showing");

    await act(async () => {
      root?.render(
        <PlayerVideoSurface
          streamUrl="/video"
          title="字幕生命周期"
          videoRef={() => undefined}
          onClick={() => undefined}
          onDoubleClick={() => undefined}
          captionTrack={{ id: "caption-1", label: "中文", src: "/caption.vtt" }}
          captionOffset={-1}
          captionDuration={5}
          centerSplash={null}
          fallbackLoading={false}
          fallbackSpeedX={null}
          errorMessage={null}
          showError={false}
          onRetry={() => undefined}
          onBack={() => undefined}
        />,
      );
    });
    expect(first).toMatchObject({ startTime: 0, endTime: 2 });

    await act(async () => root?.unmount());
    root = undefined;
    expect(first).toMatchObject({ startTime: 1, endTime: 3 });
    expect(nativeTrack.mode).toBe("disabled");
  });
});
