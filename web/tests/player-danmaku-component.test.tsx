// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../src/lib/api/client";
import { PlayerDanmakuOverlay } from "../src/features/player/components/player-danmaku-overlay";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("LuxPlayer danmaku overlay", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("loads only while visible and renders parsed danmaku as text nodes", async () => {
    const metadata = vi.spyOn(api, "webDanmaku").mockResolvedValue({
      available: true,
      format: "BILIBILI_XML",
      sourceId: "source-1",
      rawUrl: "/api/v1/items/item-1/danmaku/raw?sourceId=source-1",
    });
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response('<i><d p="0,1,25,16711680,0,0,0,0">&lt;strong&gt;safe&lt;/strong&gt;</d></i>'),
    );

    await act(async () => {
      root.render(
        <PlayerDanmakuOverlay
          itemId="item-1"
          sourceId="source-1"
          visible={false}
          currentTime={0}
          playbackRate={1}
          lifecycleKey="session-1"
        />,
      );
      await Promise.resolve();
    });
    expect(metadata).not.toHaveBeenCalled();
    expect(fetchMock).not.toHaveBeenCalled();

    await act(async () => {
      root.render(
        <PlayerDanmakuOverlay
          itemId="item-1"
          sourceId="source-1"
          visible
          currentTime={1}
          playbackRate={1}
          lifecycleKey="session-1"
        />,
      );
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(metadata).toHaveBeenCalledWith("item-1", "source-1");
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/v1/items/item-1/danmaku/raw?sourceId=source-1",
      expect.objectContaining({ credentials: "same-origin" }),
    );
    const text = container.querySelector<HTMLElement>(".lux-player-danmaku-text");
    expect(text?.textContent).toBe("<strong>safe</strong>");
    expect(text?.children).toHaveLength(0);

    await act(async () => {
      root.render(
        <PlayerDanmakuOverlay
          itemId="item-1"
          sourceId="source-1"
          visible={false}
          currentTime={1}
          playbackRate={1}
          lifecycleKey="session-1"
        />,
      );
      await Promise.resolve();
    });
    expect(container.querySelector("[data-lux-danmaku-overlay]")).toBeNull();
  });
});
