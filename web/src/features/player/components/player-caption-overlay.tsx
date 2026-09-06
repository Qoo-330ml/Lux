import { useEffect, useMemo, useRef, useState, type CSSProperties } from "react";
import {
  activeCaptionCues,
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
  const activeCues = useMemo(() => activeCaptionCues(shiftedCues, currentTime), [currentTime, shiftedCues]);

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

  if (!source || loading || activeCues.length === 0) return null;
  return (
    <div className="lux-player-caption-overlay" aria-label="字幕" aria-live="polite">
      {activeCues.map((cue) => (
        <span
          className="lux-player-caption-text"
          key={cue.id}
          style={captionPositionStyle(cue)}
        >
          {cue.runs?.length
            ? cue.runs.map((run, index) => (
              <span key={`${cue.id}-run-${index}`} style={captionRunStyle(run)}>{run.text}</span>
            ))
            : cue.text}
        </span>
      ))}
    </div>
  );
}

function captionPositionStyle(cue: { alignment?: number; position?: { x: number; y: number } }): CSSProperties {
  const style: CSSProperties = {};
  if (cue.position && Number.isFinite(cue.position.x) && Number.isFinite(cue.position.y)) {
    style.position = "absolute";
    style.left = `${Math.max(0, Math.min(100, cue.position.x))}%`;
    style.top = `${Math.max(0, Math.min(100, cue.position.y))}%`;
    style.transform = "translate(-50%, -50%)";
  } else if (cue.alignment && cue.alignment >= 1 && cue.alignment <= 9) {
    style.alignSelf = cue.alignment <= 3 ? "flex-end" : cue.alignment <= 6 ? "center" : "flex-start";
  }
  return style;
}

function captionRunStyle(run: { color?: string; bold?: boolean; italic?: boolean }): CSSProperties {
  const style: CSSProperties = {};
  if (run.color && /^rgba\(\d{1,3}, \d{1,3}, \d{1,3}, (?:0|1|0?\.\d+)\)$/u.test(run.color)) style.color = run.color;
  if (run.bold === true) style.fontWeight = 700;
  if (run.italic === true) style.fontStyle = "italic";
  return style;
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
