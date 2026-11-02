/**
 * Danmaku document loading: metadata, the same-origin raw fetch, the byte guard, and Worker parsing.
 *
 * Kept apart from parsing (pure) and rendering (rAF), so the network/worker path can be tested on its
 * own and the overlay component only has to wire state together.
 */

import { api } from "../../../lib/api/client";
import {
  DanmakuParseError,
  DANMAKU_LIMITS,
  decodeDanmakuBytes,
  parseDanmakuXml,
  type DanmakuParseResult,
} from "./parse";
import type { DanmakuWorkerResponse } from "../danmaku-worker";

/**
 * Item/source pairs the server already answered "no danmaku" for (HTTP 404, code `NOT_FOUND`).
 *
 * `GET /api/v1/items/{id}/danmaku` answers 404 when the media has no danmaku sidecar, which is a
 * normal empty state rather than an error. The overlay reloads for every playback lifecycle key
 * (session fallback, track/quality switch), so without this memo the same empty answer is requested
 * again on each of them and every request leaves a 404 in the console. The memo lives for the page
 * module, so a sidecar registered later still shows up after a reload.
 */
const sourcesWithoutDanmaku = new Set<string>();

export function danmakuSourceKey(itemId: string, sourceId: string) {
  return `${itemId}\u0000${sourceId}`;
}

export function isKnownEmptyDanmakuSource(key: string) {
  return sourcesWithoutDanmaku.has(key);
}

export function rememberEmptyDanmakuSource(key: string) {
  sourcesWithoutDanmaku.add(key);
}

/** Clears the page-level 404 memo (tests, or after a library rescan). */
export function forgetKnownEmptyDanmakuSources() {
  sourcesWithoutDanmaku.clear();
}

export function sameOriginRawUrl(rawUrl: string) {
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

/**
 * Reads a raw danmaku response as bytes, enforcing the client byte budget.
 *
 * Bytes (not a decoded string) are returned on purpose: the Worker decodes them, so the main thread
 * never holds both a byte buffer and a large JS string for the same payload.
 */
export async function readDanmakuResponse(
  response: Response,
  signal: AbortSignal,
): Promise<ArrayBuffer> {
  const declaredLength = response.headers.get("content-length");
  // The server serves up to 50 MiB, but the browser budget is 16 MiB (low-memory devices OOM well
  // before that); anything above it is refused before buffering it.
  if (declaredLength && Number(declaredLength) > DANMAKU_LIMITS.maxBytes) {
    throw new Error("弹幕文件过大");
  }
  if (!response.body) {
    const buffer = await response.arrayBuffer();
    if (buffer.byteLength > DANMAKU_LIMITS.maxBytes) {
      throw new Error("弹幕文件过大");
    }
    return buffer;
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
  // The buffer is exactly sized, so it can be transferred to the Worker without copying.
  return bytes.buffer;
}

let requestSequence = 0;

/**
 * Parses off the main thread when Workers exist, otherwise in place (tests, old browsers).
 *
 * `bytes` is transferred to the Worker, which decodes it with a fatal UTF-8 decoder; the main
 * thread only decodes on the fallback path.
 */
export function parseDanmakuPayload(
  bytes: ArrayBuffer,
  signal?: AbortSignal,
): Promise<DanmakuParseResult> {
  if (typeof Worker === "undefined") {
    try {
      return Promise.resolve(parseDanmakuXml(decodeDanmakuBytes(bytes)));
    } catch (error) {
      return Promise.reject(error);
    }
  }
  requestSequence += 1;
  const requestId = requestSequence;
  return new Promise<DanmakuParseResult>((resolve, reject) => {
    const worker = new Worker(new URL("../danmaku-worker.ts", import.meta.url), { type: "module" });
    const cleanup = () => {
      worker.terminate();
      signal?.removeEventListener("abort", onAbort);
    };
    const onAbort = () => {
      cleanup();
      reject(new DOMException("Aborted", "AbortError"));
    };
    if (signal?.aborted) {
      onAbort();
      return;
    }
    signal?.addEventListener("abort", onAbort);
    worker.onmessage = (event: MessageEvent<DanmakuWorkerResponse>) => {
      const response = event.data;
      if (response.requestId !== requestId) return;
      cleanup();
      if (response.type === "FAILED") {
        reject(new DanmakuParseError("INVALID_XML", response.message));
        return;
      }
      resolve({
        entries: response.entries,
        truncated: response.truncated,
        totalEntries: response.totalEntries,
      });
    };
    worker.onerror = () => {
      cleanup();
      reject(new Error("弹幕解析失败"));
    };
    worker.postMessage({ type: "PARSE" as const, requestId, bytes }, [bytes]);
  });
}

export type DanmakuLoadOptions = {
  itemId: string;
  sourceId: string;
  signal: AbortSignal;
};

/**
 * Loads one danmaku sidecar. Rejects with `ApiError` for metadata failures (a 404 is the caller's
 * "no danmaku" signal), or `DanmakuParseError`/`Error` for oversized or unparsable content.
 */
export async function loadDanmakuDocument({
  itemId,
  sourceId,
  signal,
}: DanmakuLoadOptions): Promise<DanmakuParseResult> {
  const info = await api.webDanmaku(itemId, sourceId);
  if (signal.aborted) throw new DOMException("Aborted", "AbortError");
  const rawUrl = sameOriginRawUrl(info.rawUrl);
  const response = await fetch(rawUrl, {
    credentials: "same-origin",
    signal,
    headers: { Accept: "application/xml, text/xml" },
  });
  if (!response.ok) throw new Error("danmaku-request-failed");
  const bytes = await readDanmakuResponse(response, signal);
  if (signal.aborted) throw new DOMException("Aborted", "AbortError");
  return parseDanmakuPayload(bytes, signal);
}

/** Status line for a successful load; `null` means "nothing the user needs to know". */
export function danmakuStatusMessage(result: DanmakuParseResult): string | null {
  if (result.totalEntries === 0) return "弹幕内容为空";
  if (result.truncated) {
    return `弹幕数量过多，已截断为均匀抽样的 ${result.entries.length} 条`;
  }
  return null;
}

/** Status line for a failed load. */
const ACTIONABLE_FAILURES = new Set([
  "弹幕文件过大",
  "弹幕内容编码无效",
  // Worker failures keep their own wording instead of degrading to the generic load message.
  "弹幕解析失败",
]);

export function danmakuFailureMessage(error: unknown): string {
  if (error instanceof DanmakuParseError) return error.message;
  if (error instanceof Error && ACTIONABLE_FAILURES.has(error.message)) return error.message;
  return "弹幕加载失败";
}
