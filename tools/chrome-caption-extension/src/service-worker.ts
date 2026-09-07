import {
  MatroskaStreamDemuxer,
  parseMatroska,
  type MatroskaFile,
  type MatroskaSample,
  type MatroskaTrack,
} from "../../../web/src/features/player/matroska-demuxer";
import {
  locateMatroskaIndex,
  parseMatroskaCuesRange,
  type MatroskaCue,
  type MatroskaRangeIndex,
} from "../../../web/src/features/player/matroska-range-index";
import { parseMatroskaSubtitleSample } from "../../../web/src/features/player/matroska-subtitles";

const PAGE_SOURCE = "lux-caption-page";
const EXTENSION_SOURCE = "lux-caption-extension";
const MEDIA_PAGE_SOURCE = "lux-media-page";
const MEDIA_EXTENSION_SOURCE = "lux-media-extension";
const PROTOCOL_VERSION = 1;
const INITIAL_RANGE_BYTES = 1 * 1024 * 1024;
const HEADER_RETRY_BYTES = 8 * 1024 * 1024;
const MAX_CUES_RANGE_BYTES = 8 * 1024 * 1024;
const MAX_CLUSTER_RANGE_BYTES = 32 * 1024 * 1024;
const LOOKAHEAD_SECONDS = 45;
const MAX_CUES = 100_000;
const MAX_TEXT = 64 * 1024;

type Selection = {
  id: string;
  name?: string;
  language?: string;
  format?: "srt" | "ass" | "ssa";
  ordinal?: number;
};

type PageMessage = {
  source: string;
  version: number;
  type: "hello" | "start" | "set-time" | "stop";
  sessionId: string;
  mediaUrl?: string;
  selection?: Selection;
  time?: number;
};

type MediaPageMessage = {
  source: string;
  version: number;
  type: "hello" | "start" | "read-range" | "stop";
  sessionId: string;
  mediaUrl?: string;
  requestId?: string;
  start?: number;
  end?: number;
};

const sessions = new Map<string, CaptionSession>();
const mediaSessions = new Map<string, { sourceUrl: string }>();
const mediaRequests = new Map<string, AbortController>();

chrome.runtime.onMessage.addListener((rawMessage, sender, sendResponse) => {
  const message = rawMessage as PageMessage | MediaPageMessage;
  const tabId = sender.tab?.id;
  if (tabId === undefined || message.version !== PROTOCOL_VERSION || typeof message.sessionId !== "string") return;
  if (message.source === MEDIA_PAGE_SOURCE) {
    void handleMediaMessage(message as MediaPageMessage, tabId, sender.tab?.url, sendResponse);
    return true;
  }
  if (message.source !== PAGE_SOURCE) return;
  if (message.type === "hello") {
    sendResponse({ source: EXTENSION_SOURCE, version: PROTOCOL_VERSION, type: "ready", sessionId: message.sessionId });
    return;
  }
  if (message.type === "start"
    && isSafeMediaUrl(message.mediaUrl)
    && isPageScopedUrl(message.mediaUrl, sender.tab?.url)
    && isSelection(message.selection)) {
    sessions.get(sessionKey(tabId, message.sessionId))?.stop();
    const session = new CaptionSession(tabId, message.sessionId, message.mediaUrl, message.selection);
    sessions.set(sessionKey(tabId, message.sessionId), session);
    void session.start().catch((error) => session.fail(error instanceof Error ? error : new Error(String(error))));
  } else if (message.type === "set-time" && Number.isFinite(message.time)) {
    sessions.get(sessionKey(tabId, message.sessionId))?.setTime(message.time as number);
  } else if (message.type === "stop") {
    const key = sessionKey(tabId, message.sessionId);
    sessions.get(key)?.stop();
    sessions.delete(key);
  }
  sendResponse({ ok: true });
});

chrome.tabs.onRemoved.addListener((tabId) => {
  for (const [key, session] of sessions) {
    if (key.startsWith(`${tabId}:`)) {
      session.stop();
      sessions.delete(key);
    }
  }
  for (const [key] of mediaSessions) {
    if (key.startsWith(`${tabId}:`)) mediaSessions.delete(key);
  }
  for (const [key, controller] of mediaRequests) {
    if (key.startsWith(`${tabId}:`)) {
      controller.abort();
      mediaRequests.delete(key);
    }
  }
});

async function handleMediaMessage(
  message: MediaPageMessage,
  tabId: number,
  pageUrl: string | undefined,
  sendResponse: (response?: unknown) => void,
) {
  const key = sessionKey(tabId, message.sessionId);
  if (message.type === "hello") {
    sendResponse({ source: MEDIA_EXTENSION_SOURCE, version: PROTOCOL_VERSION, type: "ready", sessionId: message.sessionId });
    return;
  }
  if (message.type === "start" && isSafeMediaUrl(message.mediaUrl) && isPageScopedUrl(message.mediaUrl, pageUrl)) {
    mediaSessions.set(key, { sourceUrl: message.mediaUrl });
    sendResponse({ source: MEDIA_EXTENSION_SOURCE, version: PROTOCOL_VERSION, type: "started", sessionId: message.sessionId });
    return;
  }
  if (message.type === "read-range" && typeof message.requestId === "string"
    && Number.isSafeInteger(message.start) && Number.isSafeInteger(message.end)) {
    const session = mediaSessions.get(key);
    if (!session || message.start === undefined || message.end === undefined
      || message.start < 0 || message.end < message.start || message.end - message.start + 1 > MAX_CLUSTER_RANGE_BYTES) {
      sendResponse({ source: MEDIA_EXTENSION_SOURCE, version: PROTOCOL_VERSION, type: "error", sessionId: message.sessionId, message: "扩展媒体会话或 Range 无效" });
      return;
    }
    try {
      const requestKey = `${key}:${message.requestId}`;
      const controller = new AbortController();
      mediaRequests.set(requestKey, controller);
      const range = await readRange(session.sourceUrl, message.start, message.end, controller.signal);
      const data = range.data.slice().buffer;
      sendResponse({
        source: MEDIA_EXTENSION_SOURCE,
        version: PROTOCOL_VERSION,
        type: "range",
        sessionId: message.sessionId,
        requestId: message.requestId,
        start: range.start,
        end: range.end,
        total: range.total,
        etag: range.etag,
        data,
      });
    } catch (error) {
      sendResponse({ source: MEDIA_EXTENSION_SOURCE, version: PROTOCOL_VERSION, type: "error", sessionId: message.sessionId, requestId: message.requestId, message: error instanceof Error ? error.message : String(error) });
    } finally {
      mediaRequests.delete(`${key}:${message.requestId}`);
    }
    return;
  }
  if (message.type === "stop") {
    mediaSessions.delete(key);
    for (const [requestKey, controller] of mediaRequests) {
      if (requestKey.startsWith(`${key}:`)) {
        controller.abort();
        mediaRequests.delete(requestKey);
      }
    }
    sendResponse({ ok: true });
    return;
  }
  sendResponse({ source: MEDIA_EXTENSION_SOURCE, version: PROTOCOL_VERSION, type: "error", sessionId: message.sessionId, message: "扩展媒体消息无效" });
}

class CaptionSession {
  private readonly abortController = new AbortController();
  private readonly loadedClusters = new Set<number>();
  private readonly emittedCues = new Set<string>();
  private requestedTime = 0;
  private loadPromise: Promise<void> | null = null;
  private stopped = false;
  private index: MatroskaRangeIndex | null = null;
  private parsed: MatroskaFile | null = null;
  private selectedTrack: MatroskaTrack | null = null;
  private demuxer: MatroskaStreamDemuxer | null = null;

  constructor(
    private readonly tabId: number,
    private readonly sessionId: string,
    private readonly sourceUrl: string,
    private readonly selection: Selection,
  ) {}

  async start() {
    const header = await this.readHeader();
    const initial = header.file;
    const supportedTracks = initial.subtitleTracks.filter(isSupportedSubtitleTrack);
    if (supportedTracks.length === 0) throw new Error("远程媒体没有可用的 SRT/ASS/SSA 字幕轨道");
    this.selectedTrack = selectTrack(supportedTracks, this.selection);
    if (!this.selectedTrack) throw new Error("未找到所选远程内嵌字幕轨道");
    this.send({
      type: "tracks",
      tracks: supportedTracks.map((track, ordinal) => ({
        id: matroskaTrackId(track),
        label: track.name?.trim() || track.languageBcp47?.trim() || track.language?.trim() || `字幕轨道 ${ordinal + 1}`,
        language: track.languageBcp47?.trim() || track.language?.trim() || undefined,
        format: subtitleFormat(track.codecId),
        isDefault: track.isDefault,
        isForced: track.isForced,
        ordinal,
      })),
    });
    const metadata = locateMatroskaIndex(header.data, header.total);
    const cuesEnd = Math.min(header.total - 1, metadata.cuesOffset + MAX_CUES_RANGE_BYTES - 1);
    const cuesRange = await readRange(this.sourceUrl, metadata.cuesOffset, cuesEnd, this.abortController.signal);
    const index = parseMatroskaCuesRange(
      cuesRange.data,
      cuesRange.start,
      cuesRange.total,
      metadata,
      initial.videoTrack?.number,
    );
    if (index.cues.length > MAX_CUES) throw new Error("远程媒体 CuePoint 数量超限");
    this.index = index;
    this.parsed = initial;
    this.demuxer = new MatroskaStreamDemuxer({
      onSample: (sample) => this.handleSample(sample),
      onError: (error) => this.fail(error instanceof Error ? error : new Error(String(error))),
    }, {
      tracks: [...initial.subtitleTracks, ...(initial.videoTrack ? [initial.videoTrack] : []), ...(initial.audioTrack ? [initial.audioTrack] : [])],
      timecodeScale: initial.timecodeScale,
    });
    this.setTime(0);
  }

  setTime(time: number) {
    if (this.stopped || !this.index || !this.parsed || !Number.isFinite(time)) return;
    this.requestedTime = Math.max(0, time);
    if (!this.loadPromise) {
      const requested = this.requestedTime;
      this.loadPromise = this.loadWindow(requested).finally(() => {
        this.loadPromise = null;
        if (!this.stopped && requested !== this.requestedTime) this.setTime(this.requestedTime);
      });
    }
  }

  stop() {
    this.stopped = true;
    this.abortController.abort();
    this.demuxer = null;
    this.index = null;
    this.parsed = null;
    this.loadedClusters.clear();
  }

  fail(error: Error) {
    if (this.stopped) return;
    this.send({ type: "error", message: error.message });
    this.stop();
  }

  private async loadWindow(time: number) {
    const index = this.index;
    const parsed = this.parsed;
    if (!index || !parsed) return;
    const scale = parsed.timecodeScale / 1_000_000;
    const startTick = Math.max(0, Math.floor((time - 2) * 1000 / scale));
    const endTick = Math.ceil((time + LOOKAHEAD_SECONDS) * 1000 / scale);
    const first = index.cues.reduce<MatroskaCue | null>((candidate, cue) => (
      cue.timecode <= startTick && (!candidate || cue.timecode > candidate.timecode) ? cue : candidate
    ), null);
    const startIndex = first ? index.cues.indexOf(first) : 0;
    const clusters: number[] = [];
    for (let indexPosition = startIndex; indexPosition < index.cues.length; indexPosition += 1) {
      const cue = index.cues[indexPosition];
      if (cue.timecode > endTick) break;
      if (!clusters.includes(cue.clusterOffset)) clusters.push(cue.clusterOffset);
    }
    for (const offset of clusters) {
      if (this.stopped || this.loadedClusters.has(offset)) continue;
      await this.loadCluster(offset);
    }
  }

  private async loadCluster(clusterOffset: number) {
    const index = this.index;
    if (!index) return;
    const nextOffset = index.cues.find((cue) => cue.clusterOffset > clusterOffset)?.clusterOffset;
    const end = Math.min(
      index.segmentEnd !== null ? index.segmentEnd - 1 : Number.MAX_SAFE_INTEGER,
      nextOffset !== undefined ? nextOffset - 1 : clusterOffset + MAX_CLUSTER_RANGE_BYTES - 1,
    );
    if (end < clusterOffset) return;
    const result = await readRange(this.sourceUrl, clusterOffset, end, this.abortController.signal);
    if (this.stopped || !this.demuxer) return;
    this.loadedClusters.add(clusterOffset);
    this.demuxer.write(result.data);
  }

  private handleSample(sample: MatroskaSample) {
    if (this.stopped || !this.selectedTrack || sample.trackNumber !== this.selectedTrack.number) return;
    try {
      const cue = parseMatroskaSubtitleSample(sample.data, this.selectedTrack, sample.timestampMs, sample.durationMs);
      if (!cue || cue.text.length > MAX_TEXT) return;
      const trackId = matroskaTrackId(this.selectedTrack);
      const key = `${trackId}:${cue.start}:${cue.end}:${cue.text}`;
      if (this.emittedCues.has(key)) return;
      this.emittedCues.add(key);
      this.send({ type: "cue", cue: {
        trackId,
        startMs: cue.start * 1000,
        endMs: cue.end * 1000,
        text: cue.text,
        layer: cue.layer,
        alignment: cue.alignment,
        position: cue.position,
        style: cue.style,
        runs: cue.runs,
      } });
    } catch (error) {
      this.fail(error instanceof Error ? error : new Error(String(error)));
    }
  }

  private send(message: Record<string, unknown>) {
    void chrome.tabs.sendMessage(this.tabId, {
      source: EXTENSION_SOURCE,
      version: PROTOCOL_VERSION,
      sessionId: this.sessionId,
      ...message,
    }).catch(() => undefined);
  }

  private async readHeader() {
    let result = await readRange(this.sourceUrl, 0, INITIAL_RANGE_BYTES - 1, this.abortController.signal);
    try {
      return { file: parseMatroska(result.data), data: result.data, total: result.total };
    } catch (firstError) {
      result = await readRange(this.sourceUrl, 0, HEADER_RETRY_BYTES - 1, this.abortController.signal);
      try {
        return { file: parseMatroska(result.data), data: result.data, total: result.total };
      } catch {
        throw firstError;
      }
    }
  }
}

async function readRange(url: string, start: number, end: number, signal: AbortSignal) {
  const parsed = new URL(url);
  if (!/^https?:$/u.test(parsed.protocol)) throw new Error("扩展只允许读取 HTTP(S) 媒体");
  if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || start < 0 || end < start || end - start + 1 > MAX_CLUSTER_RANGE_BYTES) {
    throw new Error("远程字幕 Range 无效");
  }
  const response = await fetch(parsed.href, {
    method: "GET",
    headers: { Range: `bytes=${start}-${end}` },
    // The signed Lux direct URL carries the one-time authorization. Never
    // forward page cookies to an arbitrary STRM/CDN host from the extension.
    credentials: "omit",
    redirect: "follow",
    cache: "no-store",
    signal,
  });
  if (response.status !== 206) throw new Error("远程媒体未提供可读取的 Range 响应");
  const contentRange = response.headers.get("content-range");
  const match = /^bytes\s+(\d+)-(\d+)\/(\d+|\*)$/iu.exec(contentRange ?? "");
  if (!match || Number(match[1]) !== start || Number(match[2]) < start || match[3] === "*") {
    throw new Error("远程媒体 Content-Range 无效");
  }
  const data = new Uint8Array(await response.arrayBuffer());
  const responseEnd = Number(match[2]);
  if (data.byteLength !== responseEnd - start + 1) throw new Error("远程媒体 Range 长度不一致");
  return { data, start, end: responseEnd, total: Number(match[3]), etag: response.headers.get("etag") };
}

function isSupportedSubtitleTrack(track: MatroskaTrack) {
  return ["S_TEXT/UTF8", "S_TEXT/ASS", "S_TEXT/SSA"].includes(track.codecId.trim().toUpperCase())
    && track.contentEncodings.every((encoding) => encoding.type === 0 && (encoding.algorithm === null || encoding.algorithm === 0));
}

function subtitleFormat(codecId: string): "srt" | "ass" | "ssa" {
  const codec = codecId.trim().toUpperCase();
  return codec === "S_TEXT/ASS" ? "ass" : codec === "S_TEXT/SSA" ? "ssa" : "srt";
}

function matroskaTrackId(track: MatroskaTrack) {
  return track.uid === null ? `mkv-track:${track.number}` : `mkv:${String(track.uid)}`;
}

function selectTrack(tracks: readonly MatroskaTrack[], selection: Selection) {
  const expectedName = selection.name?.trim().toLowerCase();
  const expectedLanguage = selection.language?.trim().toLowerCase();
  const expectedFormat = selection.format === "srt" ? "S_TEXT/UTF8" : selection.format ? `S_TEXT/${selection.format.toUpperCase()}` : null;
  return tracks.map((track, ordinal) => ({
    track,
    ordinal,
    score: (expectedName && track.name?.trim().toLowerCase() === expectedName ? 8 : 0)
      + (expectedLanguage && [track.language, track.languageBcp47].some((value) => value?.trim().toLowerCase() === expectedLanguage) ? 4 : 0)
      + (expectedFormat && track.codecId.toUpperCase() === expectedFormat ? 2 : 0)
      + (selection.ordinal === ordinal ? 1 : 0),
  })).sort((left, right) => right.score - left.score || left.ordinal - right.ordinal)[0]?.track ?? null;
}

function sessionKey(tabId: number, sessionId: string) { return `${tabId}:${sessionId}`; }

function isSafeMediaUrl(value: unknown): value is string {
  if (typeof value !== "string" || value.length === 0 || value.length > 8_192) return false;
  try {
    return /^https?:$/u.test(new URL(value).protocol);
  } catch {
    return false;
  }
}

function isPageScopedUrl(value: string, pageUrl?: string) {
  try {
    return new URL(value).origin === new URL(pageUrl ?? "").origin;
  } catch {
    return false;
  }
}

function isSelection(value: unknown): value is Selection {
  if (!value || typeof value !== "object") return false;
  const selection = value as Partial<Selection>;
  if (typeof selection.id !== "string" || selection.id.length > 256) return false;
  if (selection.name !== undefined && (typeof selection.name !== "string" || selection.name.length > 256)) return false;
  if (selection.language !== undefined && (typeof selection.language !== "string" || selection.language.length > 64)) return false;
  if (selection.format !== undefined && !["srt", "ass", "ssa"].includes(selection.format)) return false;
  return selection.ordinal === undefined || (Number.isInteger(selection.ordinal) && selection.ordinal >= 0 && selection.ordinal < 64);
}
