import { summarizePlaybackPerformance, type PlaybackCaptionController, type PlaybackCaptionCue, type PlaybackCaptionTrack, type PlaybackEngine, type PlaybackPerformance, type PlaybackSnapshot } from "./playback-engine";
import { hasClientMkvHevcRuntime } from "./playback-selection";
import { isHevcCodec } from "./media-codec";
import type { HevcRuntimeAssets } from "./hevc-playback-engine";
import { MatroskaRangeReader } from "./matroska-range-reader";
import type { ChromeMediaRangeReader } from "./chrome-caption-extension";
import { hasMatroskaSeekHead } from "./matroska-range-index";

type WorkerResponse =
  | { type: "ready" }
  | { type: "init"; initSegment: ArrayBuffer; codec: string }
  | { type: "segment"; mediaSegment: ArrayBuffer; mediaDurationMs: number; processingDurationMs: number }
  | { type: "caption-track"; trackId: string; label: string; language?: string; isDefault: boolean; isForced: boolean; ordinal: number }
  | { type: "caption"; trackId: string; startMs: number; endMs: number; text: string; layer?: number; alignment?: number; position?: { x: number; y: number }; style?: { color?: string; bold?: boolean; italic?: boolean; marginL?: number; marginR?: number; marginV?: number }; runs?: readonly { text: string; color?: string; bold?: boolean; italic?: boolean }[] }
  | { type: "done" }
  | { type: "error"; message: string };

class SourceBufferQueue {
  private chain = Promise.resolve();

  constructor(private readonly sourceBuffer: SourceBuffer) {}

  append(data: ArrayBuffer) {
    const next = this.chain.then(() => new Promise<void>((resolve, reject) => {
      const cleanup = () => {
        this.sourceBuffer.removeEventListener("updateend", onUpdateEnd);
        this.sourceBuffer.removeEventListener("error", onError);
      };
      const onUpdateEnd = () => { cleanup(); resolve(); };
      const onError = () => { cleanup(); reject(new Error("MKV MSE SourceBuffer append failed")); };
      this.sourceBuffer.addEventListener("updateend", onUpdateEnd, { once: true });
      this.sourceBuffer.addEventListener("error", onError, { once: true });
      try {
        this.sourceBuffer.appendBuffer(data);
      } catch (error) {
        cleanup();
        reject(error instanceof Error ? error : new Error(String(error)));
      }
    }));
    this.chain = next.catch(() => undefined);
    return next;
  }
}

export class ClientMkvEngine implements PlaybackEngine {
  readonly kind = "client-mkv" as const;
  performance: PlaybackPerformance | null = null;
  error: Error | null = null;
  readonly captionController: PlaybackCaptionController;
  private worker: Worker | null = null;
  private mediaSource: MediaSource | null = null;
  private objectUrl: string | null = null;
  private sourceBuffer: SourceBufferQueue | null = null;
  private abortController: AbortController | null = null;
  private generation = 0;
  private transcodedMediaDurationMs = 0;
  private transcodedProcessingDurationMs = 0;
  private readonly captionTracks = new Map<string, TextTrack>();
  private readonly captionTrackMetadata = new Map<string, PlaybackCaptionTrack>();
  private readonly captionCueSnapshot: PlaybackCaptionCue[] = [];
  private readonly captionListeners = new Set<() => void>();
  private selectedCaptionId: string | null = null;

  constructor(
    readonly element: HTMLVideoElement,
    private readonly assets: HevcRuntimeAssets,
    private readonly inputCodec = "",
    private readonly rangeReaderFactory?: (source: string) => ChromeMediaRangeReader,
  ) {
    this.captionController = {
      tracks: () => [...this.captionTrackMetadata.values()],
      select: (trackId) => {
        this.selectedCaptionId = trackId;
        this.captionTracks.forEach((track, id) => { track.mode = id === trackId ? "showing" : "disabled"; });
        this.captionListeners.forEach((listener) => listener());
      },
      cues: () => [...this.captionCueSnapshot],
      subscribe: (listener) => {
        this.captionListeners.add(listener);
        return () => this.captionListeners.delete(listener);
      },
    };
  }

  async setSource(source: string, poster?: string | null) {
    this.destroy();
    const generation = this.generation;
    const abortController = new AbortController();
    this.abortController = abortController;
    const mediaSource = new MediaSource();
    this.mediaSource = mediaSource;
    this.objectUrl = URL.createObjectURL(mediaSource);
    this.element.poster = poster ?? "";
    this.element.src = this.objectUrl;
    this.element.load();

    const openPromise = waitForMediaSourceOpen(mediaSource);
    const worker = new Worker(new URL("./mkv-transcode-worker.ts", import.meta.url), { type: "module" });
    this.worker = worker;
    let resolveReady: () => void = () => undefined;
    let rejectReady: (error: unknown) => void = () => undefined;
    const ready = new Promise<void>((resolve, reject) => { resolveReady = resolve; rejectReady = reject; });
    let resolvePlayback: () => void = () => undefined;
    let rejectPlayback: (error: unknown) => void = () => undefined;
    const playbackReady = new Promise<void>((resolve, reject) => { resolvePlayback = resolve; rejectPlayback = reject; });
    let playbackStarted = false;
    worker.onmessage = (event: MessageEvent<WorkerResponse>) => {
      void this.handleWorkerMessage(event.data, mediaSource, openPromise, resolveReady, resolvePlayback, () => playbackStarted, (value) => { playbackStarted = value; }, rejectPlayback);
    };
    worker.onerror = (event) => {
      const error = new Error(event.message || "MKV Worker 发生错误");
      this.error = error;
      rejectReady(error);
      rejectPlayback(error);
    };
    worker.postMessage({
      type: "init",
      wasmUrl: this.assets.wasmModuleUrl,
      wasmBinaryUrl: this.assets.wasmBinaryUrl,
      mode: this.sourceRequiresRemux(source) ? "hevc-remux" : "sdr",
    });

    try {
      await ready;
      const rangeReader = this.rangeReaderFactory?.(source) ?? new MatroskaRangeReader(source);
      let firstRange = true;
      for await (const chunk of rangeReader.chunks(abortController.signal)) {
        if (generation !== this.generation) return;
        if (firstRange && !hasMatroskaSeekHead(chunk.data, chunk.range.total)) {
          throw new Error("客户端媒体索引不可用：缺少有效 SeekHead");
        }
        firstRange = false;
        const copy = chunk.data.slice();
        worker.postMessage({ type: "data", data: copy.buffer }, [copy.buffer]);
      }
      worker.postMessage({ type: "flush" });
      await playbackReady;
    } catch (error) {
      this.error = error instanceof Error ? error : new Error(String(error));
      rejectReady(error);
      rejectPlayback(error);
      this.destroy();
      throw this.error;
    }
  }

  private sourceRequiresRemux(_source: string) {
    // The worker receives no ffprobe DTO. Non-HEVC Matroska tracks are already
    // browser-decodable in their Matroska-to-fMP4 form; HEVC alone needs the
    // existing WASM -> H.264 fallback when HEVC MSE is unavailable.
    return !isHevcCodec(this.inputCodec) || hasClientMkvHevcRuntime();
  }

  private async handleWorkerMessage(
    message: WorkerResponse,
    mediaSource: MediaSource,
    openPromise: Promise<void>,
    resolveReady: () => void,
    resolvePlayback: () => void,
    playbackStarted: () => boolean,
    setPlaybackStarted: (value: boolean) => void,
    rejectPlayback: (error: unknown) => void,
  ) {
    try {
      if (message.type === "ready") {
        resolveReady();
      } else if (message.type === "init") {
        await openPromise;
        const mime = `video/mp4; codecs="${message.codec}"`;
        if (typeof MediaSource.isTypeSupported === "function" && !MediaSource.isTypeSupported(mime)) {
          throw new Error(`客户端 MSE 不支持当前编码组合：${message.codec}`);
        }
        this.sourceBuffer = new SourceBufferQueue(mediaSource.addSourceBuffer(mime));
        await this.sourceBuffer.append(message.initSegment);
      } else if (message.type === "segment") {
        if (!this.sourceBuffer) throw new Error("MKV fallback 缺少 MSE 初始化片段");
        await this.sourceBuffer.append(message.mediaSegment);
        this.transcodedMediaDurationMs += message.mediaDurationMs;
        this.transcodedProcessingDurationMs = message.processingDurationMs;
        this.performance = summarizePlaybackPerformance(this.transcodedMediaDurationMs, this.transcodedProcessingDurationMs);
        this.element.dispatchEvent(new CustomEvent("lux:playback-performance", { detail: this.performance }));
        if (!playbackStarted()) {
          setPlaybackStarted(true);
          resolvePlayback();
        }
      } else if (message.type === "caption-track") {
        if (this.captionTracks.has(message.trackId)) return;
        try {
          const track = this.element.addTextTrack("subtitles", message.label, message.language ?? "");
          track.mode = "disabled";
          this.captionTracks.set(message.trackId, track);
          this.captionTrackMetadata.set(message.trackId, {
            id: message.trackId,
            label: message.label,
            language: message.language,
            isDefault: message.isDefault,
            isForced: message.isForced,
          });
          if (this.selectedCaptionId === null && message.isDefault) this.selectedCaptionId = message.trackId;
          this.element.dispatchEvent(new CustomEvent("lux:caption-track", { detail: { ...message } }));
          this.captionListeners.forEach((listener) => listener());
        } catch {
          // Some browsers expose MSE but do not allow script-created text tracks.
        }
      } else if (message.type === "caption") {
        const track = this.captionTracks.get(message.trackId);
        if (!track || typeof VTTCue === "undefined") return;
        try {
          track.addCue(new VTTCue(message.startMs / 1000, message.endMs / 1000, message.text));
          this.captionCueSnapshot.push({
            id: `${message.trackId}:${message.startMs}:${this.captionCueSnapshot.length}`,
            trackId: message.trackId,
            startMs: message.startMs,
            endMs: message.endMs,
            text: message.text,
            layer: message.layer,
            alignment: message.alignment,
            position: message.position,
            style: message.style,
            runs: message.runs,
          });
          this.element.dispatchEvent(new CustomEvent("lux:caption", { detail: message }));
          this.captionListeners.forEach((listener) => listener());
        } catch {
          // A malformed cue must not terminate audio/video playback.
        }
      } else if (message.type === "done") {
        if (mediaSource.readyState === "open") mediaSource.endOfStream();
      } else if (message.type === "error") {
        const error = new Error(message.message);
        this.error = error;
        if (playbackStarted()) this.element.dispatchEvent(new Event("error"));
        else rejectPlayback(error);
      }
    } catch (error) {
      this.error = error instanceof Error ? error : new Error(String(error));
      rejectPlayback(this.error);
    }
  }

  play() { return this.element.play(); }
  pause() { this.element.pause(); }
  seek(seconds: number) {
    if (Number.isFinite(seconds) && seconds >= 0) this.element.currentTime = seconds;
  }
  snapshot(): PlaybackSnapshot {
    return {
      currentTime: Number.isFinite(this.element.currentTime) ? Math.max(0, this.element.currentTime) : 0,
      duration: Number.isFinite(this.element.duration) ? Math.max(0, this.element.duration) : null,
      ended: this.element.ended,
    };
  }

  destroy() {
    this.generation += 1;
    this.abortController?.abort();
    this.abortController = null;
    this.worker?.postMessage({ type: "destroy" });
    this.worker?.terminate();
    this.worker = null;
    if (this.mediaSource?.readyState === "open") {
      try { this.mediaSource.endOfStream(); } catch { /* browser may already be tearing down */ }
    }
    this.mediaSource = null;
    this.sourceBuffer = null;
    this.element.pause();
    this.element.removeAttribute("src");
    this.element.removeAttribute("poster");
    this.element.load();
    if (this.objectUrl) URL.revokeObjectURL(this.objectUrl);
    this.objectUrl = null;
    this.performance = null;
    this.error = null;
    this.transcodedMediaDurationMs = 0;
    this.transcodedProcessingDurationMs = 0;
    this.captionTracks.forEach((track) => {
      track.mode = "disabled";
      for (const cue of Array.from(track.cues ?? [])) track.removeCue(cue);
    });
    this.captionTracks.clear();
    this.captionTrackMetadata.clear();
    this.captionCueSnapshot.splice(0);
    this.captionListeners.forEach((listener) => listener());
  }
}

function waitForMediaSourceOpen(mediaSource: MediaSource) {
  if (mediaSource.readyState === "open") return Promise.resolve();
  return new Promise<void>((resolve, reject) => {
    const onOpen = () => { cleanup(); resolve(); };
    const onError = () => { cleanup(); reject(new Error("MKV MediaSource 初始化失败")); };
    const cleanup = () => {
      mediaSource.removeEventListener("sourceopen", onOpen);
      mediaSource.removeEventListener("error", onError);
    };
    mediaSource.addEventListener("sourceopen", onOpen, { once: true });
    mediaSource.addEventListener("error", onError, { once: true });
  });
}
