export type PlaybackEngineKind = "native" | "client-hevc";

export type PlaybackSnapshot = {
  currentTime: number;
  duration: number | null;
  ended: boolean;
};

export type PlaybackPerformance = {
  mediaDurationMs: number;
  processingDurationMs: number;
  speedX: number;
  realtime: boolean;
};

export const PLAYBACK_PERFORMANCE_EVENT = "lux:playback-performance";

export function summarizePlaybackPerformance(mediaDurationMs: number, processingDurationMs: number): PlaybackPerformance | null {
  if (!Number.isFinite(mediaDurationMs) || mediaDurationMs <= 0 || !Number.isFinite(processingDurationMs) || processingDurationMs <= 0) return null;
  const speedX = mediaDurationMs / processingDurationMs;
  return {
    mediaDurationMs,
    processingDurationMs,
    speedX,
    realtime: speedX >= 1,
  };
}

export interface PlaybackEngine {
  readonly kind: PlaybackEngineKind;
  readonly element: HTMLVideoElement;
  readonly performance: PlaybackPerformance | null;
  readonly error: Error | null;
  setSource(source: string, poster?: string | null): Promise<void>;
  play(): Promise<void>;
  pause(): void;
  seek(seconds: number): void;
  snapshot(): PlaybackSnapshot;
  destroy(): void;
}

export class NativeVideoEngine implements PlaybackEngine {
  readonly kind = "native" as const;
  readonly performance = null;
  readonly error = null;

  constructor(readonly element: HTMLVideoElement) {}

  async setSource(source: string, poster?: string | null) {
    this.element.poster = poster ?? "";
    this.element.src = source;
    this.element.load();
  }

  play() {
    return this.element.play();
  }

  pause() {
    this.element.pause();
  }

  seek(seconds: number) {
    if (!Number.isFinite(seconds) || seconds < 0) return;
    this.element.currentTime = seconds;
  }

  snapshot(): PlaybackSnapshot {
    return {
      currentTime: Number.isFinite(this.element.currentTime) ? Math.max(0, this.element.currentTime) : 0,
      duration: Number.isFinite(this.element.duration) ? Math.max(0, this.element.duration) : null,
      ended: this.element.ended,
    };
  }

  destroy() {
    this.element.pause();
    this.element.removeAttribute("src");
    this.element.removeAttribute("poster");
    this.element.load();
  }
}
