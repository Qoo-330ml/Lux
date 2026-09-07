export const LUX_CAPTION_PAGE_SOURCE = "lux-caption-page";
export const LUX_CAPTION_EXTENSION_SOURCE = "lux-caption-extension";
export const LUX_CAPTION_PROTOCOL_VERSION = 1;

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

function randomSessionId() {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") return crypto.randomUUID();
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
}
