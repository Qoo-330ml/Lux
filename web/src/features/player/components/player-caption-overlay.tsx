import { useEffect, useMemo, useRef, useState } from "react";
import {
  activeCaptionCue,
  CAPTION_LIMITS,
  parseCaptionText,
  type LuxCaptionCue,
} from "../caption-parser";
import {
  parseCaptionWorkerRequest,
  type CaptionWorkerResponse,
} from "../caption-parser-worker";
import { offsetCaptionCues } from "../caption-offset";
import type { PlayerOverlayCaptionSource } from "./player-captions";

type PlayerCaptionOverlayProps = {
  source: PlayerOverlayCaptionSource | null;
  currentTime: number;
  captionOffset?: number;
  captionDuration?: number | null;
  lifecycleKey?: string;
  onStatusChange?: (status: string | null) => void;
};

export function PlayerCaptionOverlay({
  source,
  currentTime,
  captionOffset = 0,
  captionDuration = null,
  lifecycleKey = "",
  onStatusChange,
}: PlayerCaptionOverlayProps) {
  const [cues, setCues] = useState<LuxCaptionCue[]>([]);
  const [loading, setLoading] = useState(false);
  const generationRef = useRef(0);
  const shiftedCues = useMemo(
    () => offsetCaptionCues(cues, captionOffset, captionDuration),
    [captionDuration, captionOffset, cues],
  );
  const activeCue = useMemo(() => activeCaptionCue(shiftedCues, currentTime), [currentTime, shiftedCues]);

  useEffect(() => {
    const generation = ++generationRef.current;
    const controller = new AbortController();
    let worker: Worker | null = null;
    setCues([]);
    setLoading(false);
    if (!source) {
      return () => controller.abort();
    }
    onStatusChange?.("字幕加载中…");

    const fail = (message: string) => {
      if (generation !== generationRef.current) return;
      setLoading(false);
      setCues([]);
      onStatusChange?.(message);
    };

    const load = async () => {
      setLoading(true);
      try {
        const response = await fetch(source.src, {
          credentials: "same-origin",
          signal: controller.signal,
        });
        if (!response.ok) throw new Error("subtitle-request-failed");
        const text = await readCaptionResponse(response, controller.signal);
        if (generation !== generationRef.current) return;
        const request = {
          type: "PARSE" as const,
          requestId: generation,
          format: source.format,
          text,
        };
        const complete = (result: CaptionWorkerResponse) => {
          if (generation !== generationRef.current || result.requestId !== generation) return;
          setLoading(false);
          if (result.type === "FAILED") {
            fail(result.message);
          } else {
            setCues(result.cues);
            onStatusChange?.(result.cues.length === 0 ? "字幕内容为空" : null);
          }
        };
        if (typeof Worker === "undefined") {
          complete(parseCaptionWorkerRequest(request));
          return;
        }
        worker = new Worker(new URL("../caption-parser-worker.ts", import.meta.url), { type: "module" });
        worker.onmessage = (event: MessageEvent<CaptionWorkerResponse>) => complete(event.data);
        worker.onerror = () => fail("字幕解析失败");
        worker.postMessage(request);
      } catch (error) {
        if (!controller.signal.aborted) fail(error instanceof Error && error.message === "字幕文件过大" ? error.message : "字幕加载失败");
      }
    };
    void load();
    return () => {
      controller.abort();
      worker?.terminate();
    };
  }, [lifecycleKey, onStatusChange, source?.format, source?.id, source?.src]);

  if (!source || loading || !activeCue) return null;
  return (
    <div className="lux-player-caption-overlay" aria-label="字幕" aria-live="polite">
      <span className="lux-player-caption-text">{activeCue.text}</span>
    </div>
  );
}

async function readCaptionResponse(response: Response, signal: AbortSignal) {
  const declaredLength = response.headers.get("content-length");
  if (declaredLength && Number(declaredLength) > CAPTION_LIMITS.maxBytes) {
    throw new Error("字幕文件过大");
  }
  if (!response.body) {
    const text = await response.text();
    if (new TextEncoder().encode(text).byteLength > CAPTION_LIMITS.maxBytes) throw new Error("字幕文件过大");
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
      if (total > CAPTION_LIMITS.maxBytes) throw new Error("字幕文件过大");
      chunks.push(result.value);
    }
  } catch (error) {
    await reader.cancel().catch(() => undefined);
    throw error;
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
    throw new Error("字幕内容编码无效");
  }
}
