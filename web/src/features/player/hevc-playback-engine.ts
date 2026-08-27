import { TranscodeWorkerClient } from "@hevcjs/core";
import { createFile } from "mp4box";
import { PLAYBACK_PERFORMANCE_EVENT, summarizePlaybackPerformance, type PlaybackEngine, type PlaybackPerformance, type PlaybackSnapshot } from "./playback-engine";
import { isHevcCodec } from "./media-codec";

export type HevcRuntimeAssets = {
  workerUrl: string;
  wasmUrl: string;
  wasmModuleUrl: string;
  wasmBinaryUrl: string;
};

type Mp4Track = {
  id: number;
  type: "video" | "audio" | string;
  codec: string;
  timescale: number;
  duration: number;
  nb_samples: number;
};

export function segmentSampleCount(track: Pick<Mp4Track, "nb_samples" | "duration" | "timescale">, segmentSeconds = 2) {
  if (track.nb_samples <= 0 || track.duration <= 0 || track.timescale <= 0) return 1;
  return Math.max(1, Math.round(track.nb_samples * (segmentSeconds * track.timescale) / track.duration));
}

export function segmentSampleCountForTracks(videoTrack: Pick<Mp4Track, "nb_samples" | "duration" | "timescale">, segmentSeconds = 2) {
  const count = segmentSampleCount(videoTrack, segmentSeconds);
  return { video: count, audio: count };
}

export function copySegmentBytes(data: ArrayBuffer) {
  return new Uint8Array(data).slice();
}

export function normalizeFragmentCompositionOffsets(data: ArrayBuffer) {
  const normalized = copySegmentBytes(data);
  const view = new DataView(normalized.buffer);
  const visit = (start: number, end: number) => {
    let offset = start;
    while (offset + 8 <= end) {
      const size = view.getUint32(offset);
      const type = String.fromCharCode(normalized[offset + 4], normalized[offset + 5], normalized[offset + 6], normalized[offset + 7]);
      const headerSize = size === 1 ? 16 : 8;
      const boxEnd = size === 0 ? end : offset + size;
      if (boxEnd > end || boxEnd <= offset + headerSize) return;
      if (type === "trun" && offset + 16 <= boxEnd) {
        const version = normalized[offset + 8];
        const flags = (normalized[offset + 9] << 16) | (normalized[offset + 10] << 8) | normalized[offset + 11];
        if (version === 0 && (flags & 0x800) !== 0) {
          let cursor = offset + 16;
          const sampleCount = view.getUint32(offset + 12);
          if ((flags & 0x1) !== 0) cursor += 4;
          if ((flags & 0x4) !== 0) cursor += 4;
          for (let sample = 0; sample < sampleCount; sample += 1) {
            if ((flags & 0x100) !== 0) cursor += 4;
            if ((flags & 0x200) !== 0) cursor += 4;
            if ((flags & 0x400) !== 0) cursor += 4;
            if ((flags & 0x800) !== 0) {
              if (cursor + 4 > boxEnd) break;
              if (view.getUint32(cursor) >= 0x80000000) {
                normalized[offset + 8] = 1;
                break;
              }
              cursor += 4;
            }
          }
        }
      }
      if (type === "moof" || type === "traf") visit(offset + headerSize, boxEnd);
      offset = boxEnd;
    }
  };
  visit(0, normalized.byteLength);
  return normalized;
}

class SourceBufferQueue {
  private chain = Promise.resolve();

  constructor(private readonly sourceBuffer: SourceBuffer) {}

  append(data: Uint8Array) {
    const next = this.chain.then(() => this.appendNow(data));
    this.chain = next.catch(() => undefined);
    return next;
  }

  private appendNow(data: Uint8Array) {
    return new Promise<void>((resolve, reject) => {
      const cleanup = () => {
        this.sourceBuffer.removeEventListener("updateend", onUpdateEnd);
        this.sourceBuffer.removeEventListener("error", onError);
      };
      const onUpdateEnd = () => {
        cleanup();
        resolve();
      };
      const onError = () => {
        cleanup();
        reject(new Error("MSE SourceBuffer append failed"));
      };
      this.sourceBuffer.addEventListener("updateend", onUpdateEnd, { once: true });
      this.sourceBuffer.addEventListener("error", onError, { once: true });
      try {
        const copy = new Uint8Array(data.byteLength);
        copy.set(data);
        this.sourceBuffer.appendBuffer(copy.buffer);
      } catch (error) {
        cleanup();
        reject(error instanceof Error ? error : new Error(String(error)));
      }
    });
  }
}

export class ClientHevcEngine implements PlaybackEngine {
  readonly kind = "client-hevc" as const;
  performance: PlaybackPerformance | null = null;
  error: Error | null = null;
  private worker: TranscodeWorkerClient | null = null;
  private mediaSource: MediaSource | null = null;
  private objectUrl: string | null = null;
  private abortController: AbortController | null = null;
  private videoQueue: SourceBufferQueue | null = null;
  private audioQueue: SourceBufferQueue | null = null;
  private audioElement: HTMLAudioElement | null = null;
  private audioMediaSource: MediaSource | null = null;
  private audioObjectUrl: string | null = null;
  private audioPlayHandler: (() => void) | null = null;
  private audioPauseHandler: (() => void) | null = null;
  private audioSeekHandler: (() => void) | null = null;
  private durationSeconds: number | null = null;
  private transcodedMediaDurationMs = 0;
  private transcodedProcessingDurationMs = 0;
  private streamTask: Promise<void> | null = null;
  private generation = 0;

  constructor(
    readonly element: HTMLVideoElement,
    private readonly assets: HevcRuntimeAssets,
  ) {}

  async setSource(source: string, poster?: string | null) {
    this.destroy();
    const generation = this.generation;
    this.element.poster = poster ?? "";
    this.abortController = new AbortController();
    const mediaSource = new MediaSource();
    this.mediaSource = mediaSource;
    this.objectUrl = URL.createObjectURL(mediaSource);
    this.element.src = this.objectUrl;
    this.element.load();

    const openPromise = waitForMediaSourceOpen(mediaSource);
    const worker = new TranscodeWorkerClient(this.assets);
    this.worker = worker;
    try {
      await worker.waitReady();
      const response = await fetch(source, { credentials: "same-origin", mode: "cors", signal: this.abortController.signal });
      if (!response.ok) throw new Error(`客户端媒体读取失败：HTTP ${response.status}`);
      if (!response.body) throw new Error("客户端媒体读取失败：浏览器不支持流式读取");

      const file = createFile();
      const segmentChains = new Map<number, Promise<void>>();
      const trackKinds = new Map<number, Mp4Track["type"]>();
      let resolveInitialization: () => void = () => undefined;
      let rejectInitialization: (error: unknown) => void = () => undefined;
      const initialization = new Promise<void>((resolve, reject) => {
        resolveInitialization = resolve;
        rejectInitialization = reject;
      });
      let resolveTracksReady: () => void = () => undefined;
      let rejectTracksReady: (error: unknown) => void = () => undefined;
      const tracksReady = new Promise<void>((resolve, reject) => {
        resolveTracksReady = resolve;
        rejectTracksReady = reject;
      });
      let playbackReady = false;
      let resolvePlaybackReady: () => void = () => undefined;
      let rejectPlaybackReady: (error: unknown) => void = () => undefined;
      const playbackReadyPromise = new Promise<void>((resolve, reject) => {
        resolvePlaybackReady = () => {
          playbackReady = true;
          resolve();
        };
        rejectPlaybackReady = reject;
      });
      file.onError = (error: unknown) => {
        const failure = new Error(`MP4 解封装失败：${String(error)}`);
        rejectInitialization(failure);
        rejectTracksReady(failure);
        if (!playbackReady) rejectPlaybackReady(failure);
      };
      file.onSegment = (id: number, _user: unknown, buffer: ArrayBuffer) => {
        const previous = segmentChains.get(id) ?? Promise.resolve();
        const next = previous.then(() => this.processSegment(id, buffer, trackKinds, initialization, resolvePlaybackReady));
        segmentChains.set(id, next);
      };
      file.onReady = (rawInfo) => {
        const info = rawInfo as unknown as { tracks: Mp4Track[] };
        for (const track of info.tracks) trackKinds.set(track.id, track.type);
        void this.configureTracks(info.tracks, file, mediaSource, openPromise, worker)
          .then(() => resolveInitialization())
          .catch((error) => {
            rejectInitialization(error);
            rejectTracksReady(error);
            if (!playbackReady) rejectPlaybackReady(error);
          });
        resolveTracksReady();
      };

      this.streamTask = this.consumeSource(response.body, file, segmentChains, tracksReady, initialization, mediaSource, generation, () => playbackReady, rejectPlaybackReady);
      await playbackReadyPromise;
    } catch (error) {
      this.destroy();
      throw error;
    }
  }

  private async consumeSource(
    body: ReadableStream<Uint8Array>,
    file: ReturnType<typeof createFile>,
    segmentChains: Map<number, Promise<void>>,
    tracksReady: Promise<void>,
    initialization: Promise<void>,
    mediaSource: MediaSource,
    generation: number,
    isPlaybackReady: () => boolean,
    rejectPlaybackReady: (error: unknown) => void,
  ) {
    try {
      const reader = body.getReader();
      let offset = 0;
      while (true) {
        const chunk = await reader.read();
        if (chunk.done) break;
        if (generation !== this.generation) throw new DOMException("播放已取消", "AbortError");
        const data = chunk.value.buffer.slice(chunk.value.byteOffset, chunk.value.byteOffset + chunk.value.byteLength) as ArrayBuffer & { fileStart: number };
        data.fileStart = offset;
        offset += chunk.value.byteLength;
        file.appendBuffer(data);
      }
      file.flush();
      await tracksReady;
      await initialization;
      file.flush();
      await Promise.all(segmentChains.values());
      if (mediaSource.readyState === "open") {
        setMediaSourceDuration(mediaSource, this.durationSeconds);
        mediaSource.endOfStream();
      }
      if (this.audioMediaSource?.readyState === "open") {
        setMediaSourceDuration(this.audioMediaSource, this.durationSeconds);
        this.audioMediaSource.endOfStream();
      }
    } catch (error) {
      if (generation !== this.generation) return;
      this.error = error instanceof Error ? error : new Error(String(error));
      if (!isPlaybackReady()) {
        rejectPlaybackReady(error);
      } else {
        this.element.dispatchEvent(new Event("error"));
      }
    } finally {
      if (generation === this.generation) this.streamTask = null;
    }
  }

  private async configureTracks(
    tracks: Mp4Track[],
    file: ReturnType<typeof createFile>,
    mediaSource: MediaSource,
    openPromise: Promise<void>,
    worker: TranscodeWorkerClient,
  ) {
    const video = tracks.find((track) => track.type === "video" && isHevcCodec(track.codec));
    if (!video) throw new Error("客户端 fallback 只支持包含 HEVC 视频轨的 MP4");
    this.durationSeconds = video.timescale > 0 && video.duration > 0 ? video.duration / video.timescale : null;
    const audio = tracks.find((track) => track.type === "audio" && track.codec.startsWith("mp4a."));
    const segmentSampleCounts = segmentSampleCountForTracks(video);
    file.setSegmentOptions(video.id, "video", { nbSamples: segmentSampleCounts.video, rapAlignement: true });
    if (audio) file.setSegmentOptions(audio.id, "audio", { nbSamples: segmentSampleCounts.audio, rapAlignement: true });
    const initialSegments = file.initializeSegmentation("per-track") as Array<{ id: number; buffer: ArrayBuffer }>;
    const videoInit = initialSegments.find((segment) => segment.id === video.id)?.buffer;
    if (!videoInit) throw new Error("客户端 fallback 缺少 HEVC 初始化片段");
    const audioInit = audio ? initialSegments.find((segment) => segment.id === audio.id)?.buffer : null;
    const videoInitBytes = copySegmentBytes(videoInit);
    const audioInitBytes = audioInit ? copySegmentBytes(audioInit) : null;
    file.start();
    const transcodedInit = await worker.prepareInit(videoInitBytes);
    await openPromise;
    this.videoQueue = new SourceBufferQueue(mediaSource.addSourceBuffer(`video/mp4; codecs="${transcodedInit.codec}"`));
    await this.videoQueue.append(transcodedInit.initSegment);
    if (audio) {
      if (!audioInitBytes) throw new Error("客户端 fallback 缺少 AAC 初始化片段");
      await this.configureAudioTrack(audio.codec, audioInitBytes);
    }
  }

  private async configureAudioTrack(codec: string, initSegment: Uint8Array) {
    const audio = document.createElement("audio");
    audio.preload = "auto";
    audio.setAttribute("aria-hidden", "true");
    audio.style.display = "none";
    document.body.append(audio);
    const mediaSource = new MediaSource();
    const objectUrl = URL.createObjectURL(mediaSource);
    audio.src = objectUrl;
    audio.load();
    this.audioElement = audio;
    this.audioMediaSource = mediaSource;
    this.audioObjectUrl = objectUrl;
    this.audioPlayHandler = () => {
      audio.currentTime = Number.isFinite(this.element.currentTime) ? this.element.currentTime : 0;
      void audio.play().catch(() => undefined);
    };
    this.audioPauseHandler = () => audio.pause();
    this.audioSeekHandler = () => {
      if (Number.isFinite(this.element.currentTime)) audio.currentTime = this.element.currentTime;
    };
    this.element.addEventListener("play", this.audioPlayHandler);
    this.element.addEventListener("pause", this.audioPauseHandler);
    this.element.addEventListener("seeking", this.audioSeekHandler);
    await waitForMediaSourceOpen(mediaSource);
    this.audioQueue = new SourceBufferQueue(mediaSource.addSourceBuffer(`audio/mp4; codecs="${codec}"`));
    await this.audioQueue.append(initSegment);
  }

  private async processSegment(
    id: number,
    buffer: ArrayBuffer,
    trackKinds: Map<number, Mp4Track["type"]>,
    initialization: Promise<void>,
    resolvePlaybackReady: () => void,
  ) {
    await initialization;
    if (trackKinds.get(id) === "audio") {
      if (this.audioQueue) await this.audioQueue.append(normalizeFragmentCompositionOffsets(buffer));
      return;
    }
    if (!this.worker || !this.videoQueue) throw new Error("客户端 fallback 尚未初始化");
    const transcoded = await this.worker.processMediaSegment(normalizeFragmentCompositionOffsets(buffer));
    const stats = this.worker.lastPerfStats;
    if (stats) {
      this.transcodedMediaDurationMs += stats.segDurMs;
      this.transcodedProcessingDurationMs += stats.demuxMs + stats.decodeMs + stats.encodeMs;
      this.performance = summarizePlaybackPerformance(this.transcodedMediaDurationMs, this.transcodedProcessingDurationMs);
      this.element.dispatchEvent(new CustomEvent(PLAYBACK_PERFORMANCE_EVENT, { detail: this.performance }));
    }
    if (transcoded) {
      await this.videoQueue.append(transcoded);
      resolvePlaybackReady();
    }
  }

  play() {
    return this.element.play();
  }

  pause() {
    this.element.pause();
  }

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
    this.streamTask = null;
    this.worker?.destroy();
    this.worker = null;
    if (this.mediaSource?.readyState === "open") {
      try {
        this.mediaSource.endOfStream();
      } catch {
        // The browser may already be tearing down the MediaSource.
      }
    }
    this.mediaSource = null;
    this.videoQueue = null;
    this.audioQueue = null;
    if (this.audioPlayHandler) this.element.removeEventListener("play", this.audioPlayHandler);
    if (this.audioPauseHandler) this.element.removeEventListener("pause", this.audioPauseHandler);
    if (this.audioSeekHandler) this.element.removeEventListener("seeking", this.audioSeekHandler);
    this.audioPlayHandler = null;
    this.audioPauseHandler = null;
    this.audioSeekHandler = null;
    this.audioElement?.pause();
    this.audioElement?.removeAttribute("src");
    this.audioElement?.load();
    this.audioElement?.remove();
    if (this.audioObjectUrl) URL.revokeObjectURL(this.audioObjectUrl);
    this.audioElement = null;
    this.audioMediaSource = null;
    this.audioObjectUrl = null;
    this.durationSeconds = null;
    this.transcodedMediaDurationMs = 0;
    this.transcodedProcessingDurationMs = 0;
    this.performance = null;
    this.error = null;
    this.element.pause();
    this.element.removeAttribute("src");
    this.element.load();
    if (this.objectUrl) URL.revokeObjectURL(this.objectUrl);
    this.objectUrl = null;
  }
}

function waitForMediaSourceOpen(mediaSource: MediaSource) {
  if (mediaSource.readyState === "open") return Promise.resolve();
  return new Promise<void>((resolve, reject) => {
    const onOpen = () => {
      cleanup();
      resolve();
    };
    const onError = () => {
      cleanup();
      reject(new Error("MediaSource 初始化失败"));
    };
    const cleanup = () => {
      mediaSource.removeEventListener("sourceopen", onOpen);
      mediaSource.removeEventListener("error", onError);
    };
    mediaSource.addEventListener("sourceopen", onOpen, { once: true });
    mediaSource.addEventListener("error", onError, { once: true });
  });
}

function setMediaSourceDuration(mediaSource: MediaSource, duration: number | null) {
  if (duration === null || mediaSource.readyState !== "open") return;
  try {
    mediaSource.duration = duration;
  } catch {
    // MSE keeps the derived duration when a coded frame has an outlying timestamp.
  }
}
