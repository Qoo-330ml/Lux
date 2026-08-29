export const SINGLE_MEDIA_READ_CAPTION_EXPERIMENT_MAX_BYTES = 8 * 1024 * 1024;
export const SINGLE_MEDIA_READ_CAPTION_EXPERIMENT_TIMEOUT_MS = 10_000;

export type SingleMediaReadCaptionRequest = {
  mode: "cors" | "same-origin" | "no-cors";
  rangeRequested: boolean;
};

export type SingleMediaReadCaptionContext<T = unknown> = {
  enabled: boolean;
  sourceKind?: string | null;
  response: Response;
  body?: ReadableStream<Uint8Array> | null;
  request: SingleMediaReadCaptionRequest;
  parser?: (bytes: Uint8Array) => T | Promise<T>;
  signal?: AbortSignal;
  maxBytes?: number;
  timeoutMs?: number;
};

export type SingleMediaReadCaptionGateReason =
  | "disabled"
  | "unsupported-source"
  | "cors-unavailable"
  | "range-unavailable"
  | "media-type-unavailable"
  | "body-unavailable"
  | "parser-unavailable"
  | "byte-limit-exceeded";

export type SingleMediaReadCaptionFailureReason =
  | "byte-limit-exceeded"
  | "cancelled"
  | "timed-out"
  | "read-failed"
  | "parser-failed";

type ExperimentDiagnostics = {
  sourceKind: string;
  mediaType?: string;
  bytesRead: number;
  outcome: "skipped" | "failed" | "parsed";
  reason?: SingleMediaReadCaptionGateReason | SingleMediaReadCaptionFailureReason;
};

export type SingleMediaReadCaptionGate<T = unknown> =
  | {
      allowed: true;
      mediaType: string;
      maxBytes: number;
      body: ReadableStream<Uint8Array>;
      parser: (bytes: Uint8Array) => T | Promise<T>;
    }
  | { allowed: false; reason: SingleMediaReadCaptionGateReason };

export type SingleMediaReadCaptionResult<T> =
  | {
      status: "skipped";
      reason: SingleMediaReadCaptionGateReason;
      bytesRead: 0;
      diagnostics: ExperimentDiagnostics;
    }
  | {
      status: "failed";
      reason: SingleMediaReadCaptionFailureReason;
      bytesRead: number;
      diagnostics: ExperimentDiagnostics;
    }
  | {
      status: "parsed";
      value: T;
      bytesRead: number;
      diagnostics: ExperimentDiagnostics;
    };

const MATROSKA_MEDIA_TYPES = new Set([
  "application/matroska",
  "application/octet-stream",
  "video/matroska",
  "video/x-matroska",
  "video/webm",
]);

export function evaluateSingleMediaReadCaptionGate<T>(
  context: SingleMediaReadCaptionContext<T>,
): SingleMediaReadCaptionGate<T> {
  if (!context.enabled) return { allowed: false, reason: "disabled" };
  if (context.sourceKind?.toUpperCase() !== "STRM_URL") {
    return { allowed: false, reason: "unsupported-source" };
  }
  if (context.request.mode === "no-cors" || context.response.type === "opaque" || context.response.status === 0) {
    return { allowed: false, reason: "cors-unavailable" };
  }
  if (!context.request.rangeRequested || !supportsRangeResponse(context.response)) {
    return { allowed: false, reason: "range-unavailable" };
  }
  const mediaType = responseMediaType(context.response);
  if (!mediaType || !MATROSKA_MEDIA_TYPES.has(mediaType)) {
    return { allowed: false, reason: "media-type-unavailable" };
  }
  const body = context.body;
  if (!body) return { allowed: false, reason: "body-unavailable" };
  const parser = context.parser;
  if (typeof parser !== "function") {
    return { allowed: false, reason: "parser-unavailable" };
  }
  const maxBytes = boundedMaxBytes(context.maxBytes);
  const contentLength = responseContentLength(context.response);
  if (contentLength !== null && contentLength > maxBytes) {
    return { allowed: false, reason: "byte-limit-exceeded" };
  }
  return { allowed: true, mediaType, maxBytes, body, parser };
}

export async function runSingleMediaReadCaptionExperiment<T>(
  context: SingleMediaReadCaptionContext<T>,
): Promise<SingleMediaReadCaptionResult<T>> {
  const gate = evaluateSingleMediaReadCaptionGate(context);
  const sourceKind = context.sourceKind ?? "UNKNOWN";
  const mediaType = responseMediaType(context.response);
  if (!gate.allowed) {
    return {
      status: "skipped",
      reason: gate.reason,
      bytesRead: 0,
      diagnostics: {
        sourceKind,
        ...(mediaType ? { mediaType } : {}),
        bytesRead: 0,
        outcome: "skipped",
        reason: gate.reason,
      },
    };
  }

  if (context.signal?.aborted) return failedResult("cancelled", 0, sourceKind, gate.mediaType);

  const reader = gate.body.getReader();
  const chunks: Uint8Array[] = [];
  let bytesRead = 0;
  let abortReason: "cancelled" | "timed-out" | undefined;
  let resolveAbort: ((reason: "cancelled" | "timed-out") => void) | undefined;
  const abortPromise = new Promise<"cancelled" | "timed-out">((resolve) => {
    resolveAbort = resolve;
  });
  const abortExperiment = (reason: "cancelled" | "timed-out") => {
    if (abortReason) return;
    abortReason = reason;
    resolveAbort?.(reason);
    void reader.cancel(`caption experiment ${reason}`).catch(() => undefined);
  };
  const onAbort = () => abortExperiment("cancelled");
  context.signal?.addEventListener("abort", onAbort, { once: true });
  if (context.signal?.aborted) abortExperiment("cancelled");
  const timeout = globalThis.setTimeout(() => abortExperiment("timed-out"), boundedTimeout(context.timeoutMs));
  try {
    while (true) {
      const readPromise = reader.read();
      void readPromise.catch(() => undefined);
      const result = await Promise.race([readPromise, abortPromise]);
      if (typeof result === "string") return failedResult(result, bytesRead, sourceKind, gate.mediaType);
      if (result.done) break;
      bytesRead += result.value.byteLength;
      if (bytesRead > gate.maxBytes) {
        void reader.cancel("caption experiment byte limit exceeded").catch(() => undefined);
        return failedResult("byte-limit-exceeded", bytesRead, sourceKind, gate.mediaType);
      }
      chunks.push(result.value.slice());
    }
  } catch {
    if (abortReason) return failedResult(abortReason, bytesRead, sourceKind, gate.mediaType);
    return failedResult("read-failed", bytesRead, sourceKind, gate.mediaType);
  } finally {
    globalThis.clearTimeout(timeout);
    context.signal?.removeEventListener("abort", onAbort);
    reader.releaseLock();
  }

  const bytes = joinChunks(chunks, bytesRead);
  try {
    const value = await gate.parser(bytes);
    if (context.signal?.aborted) return failedResult("cancelled", bytesRead, sourceKind, gate.mediaType);
    return {
      status: "parsed",
      value,
      bytesRead,
      diagnostics: {
        sourceKind,
        mediaType: gate.mediaType,
        bytesRead,
        outcome: "parsed",
      },
    };
  } catch {
    return failedResult("parser-failed", bytesRead, sourceKind, gate.mediaType);
  }
}

function supportsRangeResponse(response: Response) {
  if (response.status !== 200 && response.status !== 206) return false;
  const acceptsRanges = response.headers.get("accept-ranges")?.trim().toLowerCase() === "bytes";
  const hasContentRange = response.headers.has("content-range");
  return acceptsRanges || response.status === 206 && hasContentRange;
}

function responseMediaType(response: Response) {
  const value = response.headers.get("content-type")?.split(";", 1)[0]?.trim().toLowerCase();
  return value || undefined;
}

function responseContentLength(response: Response) {
  const value = response.headers.get("content-length");
  if (!value) return null;
  const length = Number(value);
  return Number.isSafeInteger(length) && length >= 0 ? length : null;
}

function boundedMaxBytes(value: number | undefined) {
  if (value === undefined || !Number.isSafeInteger(value) || value <= 0) {
    return SINGLE_MEDIA_READ_CAPTION_EXPERIMENT_MAX_BYTES;
  }
  return Math.min(value, SINGLE_MEDIA_READ_CAPTION_EXPERIMENT_MAX_BYTES);
}

function boundedTimeout(value: number | undefined) {
  if (value === undefined || !Number.isFinite(value) || value <= 0) {
    return SINGLE_MEDIA_READ_CAPTION_EXPERIMENT_TIMEOUT_MS;
  }
  return Math.min(value, SINGLE_MEDIA_READ_CAPTION_EXPERIMENT_TIMEOUT_MS);
}

function joinChunks(chunks: readonly Uint8Array[], length: number) {
  const bytes = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return bytes;
}

function failedResult<T>(
  reason: SingleMediaReadCaptionFailureReason,
  bytesRead: number,
  sourceKind: string,
  mediaType: string,
): SingleMediaReadCaptionResult<T> {
  return {
    status: "failed",
    reason,
    bytesRead,
    diagnostics: {
      sourceKind,
      mediaType,
      bytesRead,
      outcome: "failed",
      reason,
    },
  };
}
