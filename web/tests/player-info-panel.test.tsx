// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { PlayerInfoPanel } from "../src/features/player/components/player-info-panel";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const GROUPS = [
  {
    title: "播放",
    rows: [
      { label: "播放方式", value: "服务端 HLS" },
      { label: "服务端档位", value: "档位 4 · 软件转码" },
    ],
  },
  {
    title: "输出",
    rows: [
      { label: "视频", value: "H.264 · 源 HEVC" },
      { label: "编码帧率", value: "24.0 fps" },
      { label: "编码速度", value: "1.53×" },
    ],
  },
];

describe("PlayerInfoPanel", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
    vi.restoreAllMocks();
  });

  function render(onClose = vi.fn()) {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    act(() => {
      root?.render(
        <PlayerInfoPanel
          groups={GROUPS}
          copyText={"【播放】\n播放方式：服务端 HLS"}
          onClose={onClose}
        />,
      );
    });
    return { onClose, element: container };
  }

  it("renders grouped key/value rows and stays selectable", () => {
    const { element } = render();
    const dialog = element.querySelector('[role="dialog"]');
    expect(dialog).not.toBeNull();
    expect(dialog?.getAttribute("aria-label")).toBe("播放信息");
    const headings = [...element.querySelectorAll("h3")].map((heading) => heading.textContent);
    expect(headings).toEqual(["播放", "输出"]);
    const rows = [...element.querySelectorAll(".lux-player-info-row")].map((row) => [
      row.querySelector("dt")?.textContent,
      row.querySelector("dd")?.textContent,
    ]);
    expect(rows).toEqual([
      ["播放方式", "服务端 HLS"],
      ["服务端档位", "档位 4 · 软件转码"],
      ["视频", "H.264 · 源 HEVC"],
      ["编码帧率", "24.0 fps"],
      ["编码速度", "1.53×"],
    ]);
    expect(element.querySelector(".lux-player-info-body")).not.toBeNull();
  });

  it("copies the session dump and closes from the header buttons", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    const { onClose, element } = render();

    const copyButton = element.querySelector('[aria-label="复制播放信息"]');
    expect(copyButton).not.toBeNull();
    await act(async () => {
      copyButton?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(writeText).toHaveBeenCalledWith("【播放】\n播放方式：服务端 HLS");

    const closeButton = element.querySelector('[aria-label="关闭播放信息"]');
    expect(closeButton).not.toBeNull();
    act(() => {
      closeButton?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("keeps clicks inside the panel from reaching the player surface", () => {
    const { element } = render();
    const surfaceHandler = vi.fn();
    document.body.addEventListener("click", surfaceHandler);
    const panel = element.querySelector('[role="dialog"]');
    act(() => {
      panel?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    document.body.removeEventListener("click", surfaceHandler);
    expect(surfaceHandler).not.toHaveBeenCalled();
  });
});
