// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it } from "vitest";
import { MediaInfoPanel } from "../src/features/detail/MediaInfoPanel";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("MediaInfoPanel stream details", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
  });

  it("renders ratio frame rates, source bitrate, channel layouts, and PGS subtitles", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MediaInfoPanel
          source={{
            id: "source-1",
            sourceKind: "STRM_URL",
            bitrate: 36_920_359,
            streams: [
              {
                index: 0,
                type: "VIDEO",
                codec: "h264",
                details: {
                  Width: 1920,
                  Height: 1080,
                  AverageFrameRate: "24/1",
                },
              },
              {
                index: 1,
                type: "AUDIO",
                codec: "truehd",
                details: { Channels: 8, SampleRate: "48000" },
              },
              {
                index: 2,
                type: "SUBTITLE",
                codec: "hdmv_pgs_subtitle",
                language: "chi",
                title: "简体字幕",
                isDefault: true,
              },
            ],
          }}
        />,
      );
    });

    const text = container.textContent ?? "";
    expect(text).toContain("帧率24 fps");
    expect(text).toContain("码率36.92 Mbps");
    expect(text).toContain("布局7.1");
    expect(text).toContain("PGSSUB");
  });
});
