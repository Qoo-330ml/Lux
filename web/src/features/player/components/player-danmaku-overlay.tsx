import { useEffect, useMemo, useRef, useState } from "react";
import { ApiError, api } from "../../../lib/api/client";
import {
  activeDanmaku,
  assignDanmakuLanes,
  DANMAKU_LIMITS,
  parseBilibiliDanmaku,
  type DanmakuPlacement,
  type LuxDanmakuEntry,
} from "../danmaku";
import { parseDanmakuWorkerRequest, type DanmakuWorkerResponse } from "../danmaku-worker";

type PlayerDanmakuOverlayProps = {
  itemId: string;
  sourceId: string;
  visible: boolean;
  currentTime: number;
  playbackRate: number;
  lifecycleKey: string;
  onStatusChange?: (status: string | null) => void;
};

export function PlayerDanmakuOverlay({
  itemId,
  sourceId,
  visible,
  currentTime,
  playbackRate,
  lifecycleKey,
  onStatusChange,
}: PlayerDanmakuOverlayProps) {
  const hostRef = useRef<HTMLDivElement>(null);
  const generationRef = useRef(0);
  const [entries, setEntries] = useState<LuxDanmakuEntry[]>([]);
  const [viewport, setViewport] = useState({ width: 1280, height: 720 });
  const scheduled = useMemo(
    () => assignDanmakuLanes(entries, viewport),
    [entries, viewport],
  );
  const placements = useMemo(
    () => activeDanmaku(scheduled, currentTime, viewport, playbackRate),
    [currentTime, playbackRate, scheduled, viewport],
  );

  useEffect(() => {
    const updateViewport = () => {
      const host = hostRef.current;
      if (!host) return;
      setViewport({
        width: Math.max(1, host.clientWidth || 1280),
        height: Math.max(1, host.clientHeight || 720),
      });
    };
    updateViewport();
    if (typeof ResizeObserver !== "undefined") {
      const observer = new ResizeObserver(updateViewport);
      if (hostRef.current) observer.observe(hostRef.current);
      return () => observer.disconnect();
    }
    window.addEventListener("resize", updateViewport);
    return () => window.removeEventListener("resize", updateViewport);
  }, []);

  useEffect(() => {
    const generation = ++generationRef.current;
    const controller = new AbortController();
    let worker: Worker | null = null;
    setEntries([]);
    onStatusChange?.(null);
    if (!visible || !itemId || !sourceId) return () => controller.abort();

    const isCurrent = () => generationRef.current === generation && !controller.signal.aborted;
    const fail = (message: string) => {
      if (!isCurrent()) return;
      setEntries([]);
      onStatusChange?.(message);
    };
    const complete = (response: DanmakuWorkerResponse) => {
      if (!isCurrent() || response.requestId !== generation) return;
      if (response.type === "FAILED") {
        fail(response.message);
      } else {
        setEntries(response.entries);
        onStatusChange?.(response.entries.length > 0 ? null : "弹幕内容为空");
      }
    };
    const load = async () => {
      try {
        const info = await api.webDanmaku(itemId, sourceId);
        if (!isCurrent()) return;
        const rawUrl = sameOriginRawUrl(info.rawUrl);
        const response = await fetch(rawUrl, {
          credentials: "same-origin",
          signal: controller.signal,
          headers: { Accept: "application/xml, text/xml" },
        });
        if (!response.ok) throw new Error("danmaku-request-failed");
        const xml = await readDanmakuResponse(response, controller.signal);
        if (!isCurrent()) return;
        const request = { type: "PARSE" as const, requestId: generation, xml };
        if (typeof Worker === "undefined") {
          complete(parseDanmakuWorkerRequest(request));
          return;
        }
        worker = new Worker(new URL("../danmaku-worker.ts", import.meta.url), { type: "module" });
        worker.onmessage = (event: MessageEvent<DanmakuWorkerResponse>) => complete(event.data);
        worker.onerror = () => fail("弹幕解析失败");
        worker.postMessage(request);
      } catch (error) {
        if (!controller.signal.aborted) {
          if (error instanceof ApiError && error.status === 404) return;
          fail(error instanceof Error && error.message === "弹幕文件过大" ? error.message : "弹幕加载失败");
        }
      }
    };
    void load();
    return () => {
      controller.abort();
      worker?.terminate();
    };
  }, [itemId, lifecycleKey, onStatusChange, sourceId, visible]);

  if (!visible) return null;
  return (
    <div ref={hostRef} className="lux-player-danmaku-overlay" data-lux-danmaku-overlay aria-hidden="true">
      {placements.map((placement) => <DanmakuText key={placement.id} placement={placement} />)}
    </div>
  );
}

function DanmakuText({ placement }: { placement: DanmakuPlacement }) {
  const style = {
    color: placement.color,
    fontSize: `${placement.fontSize}px`,
    left: `${placement.x}px`,
    top: `${placement.y}px`,
  };
  return (
    <span
      className={`lux-player-danmaku-text lux-player-danmaku-${placement.mode}`}
      style={style}
      data-danmaku-id={placement.id}
    >
      {placement.text}
    </span>
  );
}

function sameOriginRawUrl(rawUrl: string) {
  const origin = typeof window === "undefined" ? "http://localhost" : window.location.origin;
  const url = new URL(rawUrl, origin);
  if (
    url.origin !== origin
    || !url.pathname.startsWith("/api/v1/items/")
    || !url.pathname.endsWith("/danmaku/raw")
  ) {
    throw new Error("danmaku URL is not same-origin");
  }
  return `${url.pathname}${url.search}`;
}

async function readDanmakuResponse(response: Response, signal: AbortSignal) {
  const declaredLength = response.headers.get("content-length");
  if (declaredLength && Number(declaredLength) > DANMAKU_LIMITS.maxBytes) {
    throw new Error("弹幕文件过大");
  }
  if (!response.body) {
    const text = await response.text();
    if (new TextEncoder().encode(text).byteLength > DANMAKU_LIMITS.maxBytes) {
      throw new Error("弹幕文件过大");
    }
    return text;
  }
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  try {
    while (true) {
      if (signal.aborted) throw new DOMException("Aborted", "AbortError");
      const result = await reader.read();
      if (result.done) break;
      total += result.value.byteLength;
      if (total > DANMAKU_LIMITS.maxBytes) throw new Error("弹幕文件过大");
      chunks.push(result.value);
    }
  } finally {
    reader.releaseLock();
  }
  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    throw new Error("弹幕内容编码无效");
  }
}
