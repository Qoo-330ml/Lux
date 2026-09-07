export const LUX_CAPTION_PAGE_SOURCE = "lux-caption-page";
export const LUX_CAPTION_EXTENSION_SOURCE = "lux-caption-extension";
export const LUX_CAPTION_PROTOCOL_VERSION = 1;
export const LUX_MEDIA_PAGE_SOURCE = "lux-media-page";
export const LUX_MEDIA_EXTENSION_SOURCE = "lux-media-extension";
export const LUX_MEDIA_PROTOCOL_VERSION = 1;

export type ChromeCaptionTrack = {
  id: string;
  label: string;
  language?: string;
  format: "srt" | "ass" | "ssa";
  isDefault: boolean;
  isForced: boolean;
  ordinal: number;
};

export type ChromeCaptionSelection = {
  id: string;
  name?: string;
  language?: string;
  format?: "srt" | "ass" | "ssa";
  ordinal?: number;
};

export type ChromeCaptionCue = {
  trackId: string;
  startMs: number;
  endMs: number;
  text: string;
  layer?: number;
  alignment?: number;
  position?: { x: number; y: number };
  style?: { color?: string; bold?: boolean; italic?: boolean; marginL?: number; marginR?: number; marginV?: number };
  runs?: readonly { text: string; color?: string; bold?: boolean; italic?: boolean }[];
};

type ChromeCaptionMessage = {
  source: string;
  version: number;
  type: "ready" | "tracks" | "cue" | "error";
  sessionId: string;
  tracks?: ChromeCaptionTrack[];
  cue?: ChromeCaptionCue;
  message?: string;
};

type ChromeCaptionOptions = {
  onTracks: (tracks: ChromeCaptionTrack[]) => void;
  onCue: (cue: ChromeCaptionCue) => void;
  onReady?: () => void;
  onError?: (error: Error) => void;
};

/**
 * Page-side bridge for the optional MV3 extension. The page never fetches the
 * media bytes itself; it only sends a signed/direct media URL and receives
 * safe caption records from the extension content script.
 */
export class ChromeCaptionExtension {
  private readonly sessionId = randomSessionId();
  private readonly onTracks: ChromeCaptionOptions["onTracks"];
  private readonly onCue: ChromeCaptionOptions["onCue"];
  private readonly onReady?: ChromeCaptionOptions["onReady"];
  private readonly onError?: ChromeCaptionOptions["onError"];
  private started = false;
  private readyTimer: number | null = null;

  constructor(options: ChromeCaptionOptions) {
    this.onTracks = options.onTracks;
    this.onCue = options.onCue;
    this.onReady = options.onReady;
    this.onError = options.onError;
  }

  start(source: string, selection: ChromeCaptionSelection) {
    if (!isHttpUrl(source)) throw new Error("扩展字幕源必须是 HTTP(S) URL");
    this.started = true;
    window.addEventListener("message", this.handleMessage);
    this.post({ type: "hello" });
    this.readyTimer = window.setTimeout(() => {
      if (this.started) this.onError?.(new Error("未检测到 Lux 字幕 Chrome 扩展"));
    }, 1_500);
    this.pendingStart = { source, selection };
  }

  private pendingStart: { source: string; selection: ChromeCaptionSelection } | null = null;

  setTime(time: number) {
    if (!this.started || !Number.isFinite(time)) return;
    this.post({ type: "set-time", time });
  }

  stop() {
    if (!this.started) return;
    this.post({ type: "stop" });
    this.started = false;
    this.pendingStart = null;
    if (this.readyTimer !== null) window.clearTimeout(this.readyTimer);
    this.readyTimer = null;
    window.removeEventListener("message", this.handleMessage);
  }

  private readonly handleMessage = (event: MessageEvent<ChromeCaptionMessage>) => {
    if (event.source !== window || event.origin !== window.location.origin) return;
    const message = event.data;
    if (!message || message.source !== LUX_CAPTION_EXTENSION_SOURCE
      || message.version !== LUX_CAPTION_PROTOCOL_VERSION || message.sessionId !== this.sessionId) return;
    if (message.type === "ready") {
      if (this.readyTimer !== null) window.clearTimeout(this.readyTimer);
      this.readyTimer = null;
      this.onReady?.();
      const pending = this.pendingStart;
      this.pendingStart = null;
      if (pending) this.post({ type: "start", mediaUrl: pending.source, selection: pending.selection });
    } else if (message.type === "tracks") {
      this.onTracks(message.tracks ?? []);
    } else if (message.type === "cue" && message.cue) {
      this.onCue(message.cue);
    } else if (message.type === "error") {
      this.onError?.(new Error(message.message || "扩展字幕读取失败"));
    }
  };

  private post(message: Record<string, unknown>) {
    window.postMessage({
      source: LUX_CAPTION_PAGE_SOURCE,
      version: LUX_CAPTION_PROTOCOL_VERSION,
      sessionId: this.sessionId,
      ...message,
    }, window.location.origin);
  }
}

export function isHttpUrl(value: string | null | undefined): value is string {
  try {
    const protocol = new URL(value ?? "").protocol;
    return protocol === "http:" || protocol === "https:";
  } catch {
    return false;
  }
}

export type ChromeMediaRange = {
  data: Uint8Array;
  start: number;
  end: number;
  total: number;
  etag: string | null;
};

export type ChromeMediaRangeReader = {
  chunks(signal?: AbortSignal): AsyncGenerator<{ data: Uint8Array; range: ChromeMediaRange }, void, void>;
};

type ChromeMediaRangeMessage = {
  source: string;
  version: number;
  type: "ready" | "started" | "range" | "error";
  sessionId: string;
  requestId?: string;
  message?: string;
  data?: ArrayBuffer;
  start?: number;
  end?: number;
  total?: number;
  etag?: string | null;
};

const MEDIA_INITIAL_RANGE_BYTES = 1 * 1024 * 1024;
const MEDIA_MAX_RANGE_BYTES = 32 * 1024 * 1024;

/**
 * Page-side bridge for the optional full-media extension engine. The page
 * never calls fetch for the remote source; every range is requested from the
 * extension and is consumed by the existing Matroska Worker/MSE pipeline.
 */
export class ChromeMediaExtension {
  private readonly sessionId = randomSessionId();
  private started = false;
  private source: string | null = null;
  private readyPromise: Promise<void> | null = null;
  private readyResolve: (() => void) | null = null;
  private readyReject: ((error: unknown) => void) | null = null;
  private readyTimer: number | null = null;
  private readonly pending = new Map<string, {
    resolve: (range: ChromeMediaRange) => void;
    reject: (error: unknown) => void;
  }>();

  async start(source: string) {
    if (!isHttpUrl(source)) throw new Error("扩展媒体源必须是 HTTP(S) URL");
    if (this.started && this.source === source) return;
    this.stop();
    this.started = true;
    this.source = source;
    window.addEventListener("message", this.handleMessage);
    this.readyPromise = new Promise<void>((resolve, reject) => {
      this.readyResolve = resolve;
      this.readyReject = reject;
    });
    this.readyTimer = window.setTimeout(() => {
      this.readyReject?.(new Error("未检测到 Lux 全媒体 Chrome 扩展"));
      this.clearReady();
    }, 1_500);
    this.post({ type: "hello" });
    await this.readyPromise;
    this.post({ type: "start", mediaUrl: source });
  }

  createRangeReader(source: string): ChromeMediaRangeReader {
    if (!this.started || this.source !== source) throw new Error("扩展媒体会话尚未启动");
    return new ExtensionRangeReader(this);
  }

  readRange(start: number, end: number, signal?: AbortSignal) {
    if (!this.started) return Promise.reject(new Error("扩展媒体会话已停止"));
    if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || start < 0 || end < start || end - start + 1 > MEDIA_MAX_RANGE_BYTES) {
      return Promise.reject(new Error("扩展媒体 Range 边界无效"));
    }
    const requestId = randomSessionId();
    return new Promise<ChromeMediaRange>((resolve, reject) => {
      const onAbort = () => {
        this.pending.delete(requestId);
        reject(new DOMException("The operation was aborted", "AbortError"));
      };
      if (signal?.aborted) {
        onAbort();
        return;
      }
      signal?.addEventListener("abort", onAbort, { once: true });
      this.pending.set(requestId, {
        resolve: (range) => {
          signal?.removeEventListener("abort", onAbort);
          resolve(range);
        },
        reject: (error) => {
          signal?.removeEventListener("abort", onAbort);
          reject(error);
        },
      });
      this.post({ type: "read-range", requestId, start, end });
    });
  }

  stop() {
    if (!this.started) return;
    this.post({ type: "stop" });
    this.started = false;
    this.source = null;
    this.pending.forEach(({ reject }) => reject(new Error("扩展媒体会话已停止")));
    this.pending.clear();
    this.clearReady();
    window.removeEventListener("message", this.handleMessage);
  }

  private readonly handleMessage = (event: MessageEvent<ChromeMediaRangeMessage>) => {
    if (event.source !== window || event.origin !== window.location.origin) return;
    const message = event.data;
    if (!message || message.source !== LUX_MEDIA_EXTENSION_SOURCE
      || message.version !== LUX_MEDIA_PROTOCOL_VERSION || message.sessionId !== this.sessionId) return;
    if (message.type === "ready") {
      const resolve = this.readyResolve;
      this.clearReady();
      resolve?.();
    } else if (message.type === "range" && message.requestId) {
      const request = this.pending.get(message.requestId);
      if (!request) return;
      this.pending.delete(message.requestId);
      const range = validateChromeMediaRange(message);
      if (!range) request.reject(new Error("扩展返回的媒体 Range 无效"));
      else request.resolve(range);
    } else if (message.type === "error") {
      const error = new Error(message.message || "扩展媒体读取失败");
      this.pending.forEach(({ reject }) => reject(error));
      this.pending.clear();
    }
  };

  private post(message: Record<string, unknown>) {
    window.postMessage({
      source: LUX_MEDIA_PAGE_SOURCE,
      version: LUX_MEDIA_PROTOCOL_VERSION,
      sessionId: this.sessionId,
      ...message,
    }, window.location.origin);
  }

  private clearReady() {
    if (this.readyTimer !== null) window.clearTimeout(this.readyTimer);
    this.readyTimer = null;
    this.readyPromise = null;
    this.readyResolve = null;
    this.readyReject = null;
  }
}

class ExtensionRangeReader implements ChromeMediaRangeReader {
  private totalLength: number | null = null;
  private etag: string | null = null;

  constructor(private readonly extension: ChromeMediaExtension) {}

  async *chunks(signal?: AbortSignal) {
    let start = 0;
    let first = true;
    while (this.totalLength === null || start < this.totalLength) {
      const end = Math.min(
        (this.totalLength ?? Number.MAX_SAFE_INTEGER) - 1,
        start + (first ? MEDIA_INITIAL_RANGE_BYTES : MEDIA_MAX_RANGE_BYTES) - 1,
      );
      const range = await this.readRange(start, end, signal);
      yield { data: range.data, range };
      start = range.end + 1;
      first = false;
      if (start > range.total) throw new Error("扩展媒体 Range 边界无效");
    }
  }

  private async readRange(start: number, end: number, signal?: AbortSignal) {
    const range = await this.extension.readRange(start, end, signal);
    if (this.totalLength === null) this.totalLength = range.total;
    if (range.total !== this.totalLength) throw new Error("扩展媒体长度发生变化");
    if (this.etag !== null && range.etag !== this.etag) throw new Error("扩展媒体 ETag 发生变化");
    this.etag ??= range.etag;
    if (range.start !== start || range.end < start || range.end > end || range.total <= range.end) {
      throw new Error("扩展媒体 Range 边界无效");
    }
    return range;
  }
}

export function validateChromeMediaRange(message: Pick<ChromeMediaRangeMessage, "data" | "start" | "end" | "total" | "etag">): ChromeMediaRange | null {
  if (!(message.data instanceof ArrayBuffer)) return null;
  if (![message.start, message.end, message.total].every((value) => Number.isSafeInteger(value))) return null;
  const start = message.start as number;
  const end = message.end as number;
  const total = message.total as number;
  if (start < 0 || end < start || total <= end) return null;
  const data = new Uint8Array(message.data);
  if (data.byteLength !== end - start + 1) return null;
  return { data, start, end, total, etag: typeof message.etag === "string" ? message.etag : null };
}

function randomSessionId() {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") return crypto.randomUUID();
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
}
