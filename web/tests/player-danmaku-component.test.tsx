// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ApiError, api } from "../src/lib/api/client";
import { PlayerDanmakuOverlay } from "../src/features/player/components/player-danmaku-overlay";
import { forgetKnownEmptyDanmakuSources } from "../src/features/player/danmaku";
import {
  parseDanmakuWorkerRequest,
  type DanmakuWorkerRequest,
} from "../src/features/player/danmaku-worker";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const RAW_URL = "/api/v1/items/item-1/danmaku/raw?sourceId=source-1";

function mockDanmakuInfo(rawUrl = RAW_URL) {
  return vi.spyOn(api, "webDanmaku").mockResolvedValue({
    available: true,
    format: "BILIBILI_XML",
    sourceId: "source-1",
    rawUrl,
  });
}

function settlement() {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

describe("LuxPlayer danmaku overlay", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    forgetKnownEmptyDanmakuSources();
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it("loads only while visible and renders parsed danmaku as text nodes", async () => {
    const metadata = mockDanmakuInfo();
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response('<?xml version="1.0" encoding="UTF-8"?>\n<i><d p="0,1,25,16711680,0,0,0,0">&lt;strong&gt;safe&lt;/strong&gt;</d></i>'),
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
      await settlement();
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
      await settlement();
    });

    expect(metadata).toHaveBeenCalledWith("item-1", "source-1");
    expect(fetchMock).toHaveBeenCalledWith(
      RAW_URL,
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
      await settlement();
    });
    expect(container.querySelector("[data-lux-danmaku-overlay]")).toBeNull();
  });

  it('treats a 404 "no danmaku" answer as an empty result and asks only once per source', async () => {
    // The real backend answers 404 (`NOT_FOUND`, “当前媒体源没有可用弹幕”) when a source has no
    // danmaku sidecar. That is a normal empty state: no status message, no failure banner, and no
    // repeated request on the next playback lifecycle (session fallback, track/quality switch).
    const metadata = vi.spyOn(api, "webDanmaku").mockRejectedValue(
      new ApiError("当前媒体源没有可用弹幕", { code: "NOT_FOUND", status: 404 }),
    );
    const fetchMock = vi.spyOn(globalThis, "fetch");
    const onStatusChange = vi.fn();

    const render = async (lifecycleKey: string) => {
      await act(async () => {
        root.render(
          <PlayerDanmakuOverlay
            itemId="item-without-danmaku"
            sourceId="source-without-danmaku"
            visible
            currentTime={0}
            playbackRate={1}
            lifecycleKey={lifecycleKey}
            onStatusChange={onStatusChange}
          />,
        );
        await settlement();
      });
    };

    await render("session-1");

    expect(metadata).toHaveBeenCalledTimes(1);
    expect(fetchMock).not.toHaveBeenCalled();
    expect(container.querySelector("[data-lux-danmaku-overlay]")).not.toBeNull();
    expect(container.querySelector(".lux-player-danmaku-text")).toBeNull();
    expect(onStatusChange).toHaveBeenCalledWith(null);
    expect(onStatusChange).not.toHaveBeenCalledWith("弹幕加载失败");

    // A replacement session re-runs the overlay effect with a new lifecycle key: the empty answer is
    // already known, so nothing is requested again.
    await render("session-2");

    expect(metadata).toHaveBeenCalledTimes(1);
    expect(onStatusChange).not.toHaveBeenCalledWith("弹幕加载失败");
  });

  it("reports a truncation status for a file with more entries than the render budget", async () => {
    mockDanmakuInfo();
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(`<i>${'<d p="1,1,25,16777215,0,0,0,0">弹幕</d>'.repeat(60_000)}</i>`),
    );
    const onStatusChange = vi.fn();

    await act(async () => {
      root.render(
        <PlayerDanmakuOverlay
          itemId="item-truncated"
          sourceId="source-1"
          visible
          currentTime={1}
          playbackRate={1}
          lifecycleKey="session-1"
          onStatusChange={onStatusChange}
        />,
      );
      await settlement();
      await settlement();
    });

    const statuses = onStatusChange.mock.calls.map(([status]) => status);
    expect(statuses).toContain(null);
    const truncation = statuses.find((status) => typeof status === "string" && status.includes("已截断"));
    expect(truncation).toBeTruthy();
    expect(truncation).toContain("抽样");
    // The file is still rendered (sampled) instead of being rejected as a whole.
    expect(container.querySelectorAll(".lux-player-danmaku-text").length).toBeGreaterThan(0);
    expect(container.querySelectorAll(".lux-player-danmaku-text").length).toBeLessThanOrEqual(80);
  });

  it("reports an empty-content status when the file carries no usable danmaku", async () => {
    mockDanmakuInfo();
    vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response("<i></i>"));
    const onStatusChange = vi.fn();

    await act(async () => {
      root.render(
        <PlayerDanmakuOverlay
          itemId="item-empty"
          sourceId="source-1"
          visible
          currentTime={1}
          playbackRate={1}
          lifecycleKey="session-1"
          onStatusChange={onStatusChange}
        />,
      );
      await settlement();
    });

    expect(onStatusChange).toHaveBeenCalledWith("弹幕内容为空");
    expect(container.querySelector("[data-lux-danmaku-overlay]")).not.toBeNull();
    expect(container.querySelector(".lux-player-danmaku-text")).toBeNull();
  });

  it("surfaces a load failure instead of silently rendering nothing", async () => {
    vi.spyOn(api, "webDanmaku").mockRejectedValue(new Error("boom"));
    const fetchMock = vi.spyOn(globalThis, "fetch");
    const onStatusChange = vi.fn();

    await act(async () => {
      root.render(
        <PlayerDanmakuOverlay
          itemId="item-failure"
          sourceId="source-1"
          visible
          currentTime={1}
          playbackRate={1}
          lifecycleKey="session-1"
          onStatusChange={onStatusChange}
        />,
      );
      await settlement();
    });

    expect(fetchMock).not.toHaveBeenCalled();
    expect(onStatusChange).toHaveBeenCalledWith("弹幕加载失败");
  });

  it("refuses a non-same-origin raw URL without fetching it", async () => {
    mockDanmakuInfo("https://evil.example/api/v1/items/item-1/danmaku/raw");
    const fetchMock = vi.spyOn(globalThis, "fetch");
    const onStatusChange = vi.fn();

    await act(async () => {
      root.render(
        <PlayerDanmakuOverlay
          itemId="item-cross-origin"
          sourceId="source-1"
          visible
          currentTime={1}
          playbackRate={1}
          lifecycleKey="session-1"
          onStatusChange={onStatusChange}
        />,
      );
      await settlement();
    });

    expect(fetchMock).not.toHaveBeenCalled();
    expect(onStatusChange).toHaveBeenCalledWith("弹幕加载失败");
  });

  it("keeps the 16 MiB client budget and reports an oversized file", async () => {
    // The server serves up to 50 MiB, but buffering/decoding that on a low-memory device risks OOM.
    mockDanmakuInfo();
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response('<?xml version="1.0"?><i></i>', {
        headers: { "content-length": String(17 * 1024 * 1024) },
      }),
    );
    const onStatusChange = vi.fn();

    await act(async () => {
      root.render(
        <PlayerDanmakuOverlay
          itemId="item-huge"
          sourceId="source-1"
          visible
          currentTime={1}
          playbackRate={1}
          lifecycleKey="session-1"
          onStatusChange={onStatusChange}
        />,
      );
      await settlement();
    });

    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(onStatusChange).toHaveBeenCalledWith("弹幕文件过大");
  });

  it("hands raw bytes to the Worker as a transferable instead of a decoded string", async () => {
    mockDanmakuInfo();
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response('<i><d p="0,1,25,16777215,0,0,0,0">Worker 字节</d></i>'),
    );

    class RecordingWorker {
      static instances: RecordingWorker[] = [];
      onmessage: ((event: MessageEvent) => void) | null = null;
      onerror: (() => void) | null = null;
      posted: Array<{ message: DanmakuWorkerRequest; transfer?: Transferable[] }> = [];
      terminated = false;

      constructor() {
        RecordingWorker.instances.push(this);
      }

      postMessage(message: DanmakuWorkerRequest, transfer?: Transferable[]) {
        this.posted.push({ message, transfer });
        const response = parseDanmakuWorkerRequest(message);
        queueMicrotask(() => this.onmessage?.({ data: response } as MessageEvent));
      }

      terminate() {
        this.terminated = true;
      }
    }
    vi.stubGlobal("Worker", RecordingWorker);

    await act(async () => {
      root.render(
        <PlayerDanmakuOverlay
          itemId="item-worker"
          sourceId="source-1"
          visible
          currentTime={1}
          playbackRate={1}
          lifecycleKey="session-1"
        />,
      );
      await settlement();
      await settlement();
    });

    const worker = RecordingWorker.instances[0];
    expect(worker).toBeDefined();
    expect(worker!.posted).toHaveLength(1);
    const { message, transfer } = worker!.posted[0]!;
    // The main thread must not decode the payload: only bytes cross the boundary, by transfer.
    expect(message.type).toBe("PARSE");
    expect(message.bytes).toBeInstanceOf(ArrayBuffer);
    expect("xml" in message).toBe(false);
    expect(transfer).toEqual([message.bytes]);
    expect(container.querySelector(".lux-player-danmaku-text")?.textContent).toBe("Worker 字节");
    expect(worker!.terminated).toBe(true);
  });

  it("reports a payload that is not valid UTF-8", async () => {
    mockDanmakuInfo();
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(new Uint8Array([0xff, 0xfe, 0x00])),
    );
    const onStatusChange = vi.fn();

    await act(async () => {
      root.render(
        <PlayerDanmakuOverlay
          itemId="item-bad-utf8"
          sourceId="source-1"
          visible
          currentTime={1}
          playbackRate={1}
          lifecycleKey="session-1"
          onStatusChange={onStatusChange}
        />,
      );
      await settlement();
    });

    expect(onStatusChange).toHaveBeenCalledWith("弹幕内容编码无效");
  });

  it("surfaces a broken Worker as a parse failure, not a generic load failure", async () => {
    mockDanmakuInfo();
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response('<i><d p="0,1,25,16777215,0,0,0,0">x</d></i>'),
    );
    const onStatusChange = vi.fn();

    class BrokenWorker {
      onmessage: ((event: MessageEvent) => void) | null = null;
      onerror: (() => void) | null = null;
      postMessage() {
        queueMicrotask(() => this.onerror?.());
      }
      terminate() {
        // no-op
      }
    }
    vi.stubGlobal("Worker", BrokenWorker);

    await act(async () => {
      root.render(
        <PlayerDanmakuOverlay
          itemId="item-broken-worker"
          sourceId="source-1"
          visible
          currentTime={1}
          playbackRate={1}
          lifecycleKey="session-1"
          onStatusChange={onStatusChange}
        />,
      );
      await settlement();
      await settlement();
    });

    expect(onStatusChange).toHaveBeenCalledWith("弹幕解析失败");
  });

  it("keeps the existing props contract without the optional danmaku additions", async () => {
    mockDanmakuInfo();
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response('<i><d p="0,5,25,16777215,0,0,0,0">顶部</d></i>'),
    );

    await act(async () => {
      root.render(
        <PlayerDanmakuOverlay
          itemId="item-legacy"
          sourceId="source-1"
          visible
          currentTime={0.5}
          playbackRate={1.5}
          lifecycleKey="session-1"
        />,
      );
      await settlement();
    });

    const node = container.querySelector<HTMLElement>(".lux-player-danmaku-top");
    expect(node?.textContent).toBe("顶部");
    expect(node?.style.transform).toContain("translate3d");
  });
});
