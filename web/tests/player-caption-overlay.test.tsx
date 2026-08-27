// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { PlayerCaptionOverlay } from "../src/features/player/components/player-caption-overlay";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("LuxPlayer caption overlay timing", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
    vi.unstubAllGlobals();
  });

  it("shows a loading status and reapplies offset from original cue times", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue({
      ok: true,
      headers: new Headers({ "content-length": "50" }),
      body: null,
      text: async () => "1\n00:00:01,000 --> 00:00:02,000\n字幕内容",
    }));
    const statuses: Array<string | null> = [];
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    await act(async () => {
      root?.render(
        <PlayerCaptionOverlay
          source={{ id: "caption-1", label: "中文", format: "srt", src: "/caption.srt" }}
          currentTime={1.6}
          captionOffset={0.5}
          captionDuration={10}
          onStatusChange={(status) => statuses.push(status)}
        />,
      );
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(statuses).toContain("字幕加载中…");
    expect(statuses.at(-1)).toBeNull();
    expect(container.querySelector(".lux-player-caption-text")?.textContent).toBe("字幕内容");

    await act(async () => {
      root?.render(
        <PlayerCaptionOverlay
          source={{ id: "caption-1", label: "中文", format: "srt", src: "/caption.srt" }}
          currentTime={1.6}
          captionOffset={1}
          captionDuration={10}
        />,
      );
    });
    expect(container.querySelector(".lux-player-caption-text")).toBeNull();

    await act(async () => {
      root?.render(
        <PlayerCaptionOverlay
          source={{ id: "caption-1", label: "中文", format: "srt", src: "/caption.srt" }}
          currentTime={1.6}
          captionOffset={0.5}
          captionDuration={1.75}
        />,
      );
    });
    expect(container.querySelector(".lux-player-caption-text")?.textContent).toBe("字幕内容");
  });
});
