import {
  EMPTY_LUX_PLAYBACK_SNAPSHOT,
  type LuxPlaybackEngineEvent,
  type LuxPlaybackEngineKind,
  type LuxPlaybackSnapshot,
  type LuxPlaybackSource,
} from "./types";

export type { LuxPlaybackEngineEvent } from "./types";

export interface LuxPlaybackEngine {
  readonly kind: LuxPlaybackEngineKind;
  readonly element: HTMLVideoElement;
  readonly performance: unknown;
  readonly error: Error | null;
  setSource(source: LuxPlaybackSource): Promise<void>;
  play(): Promise<void>;
  pause(): void;
  seek(seconds: number): void;
  snapshot(): LuxPlaybackSnapshot;
  subscribe(listener: (event: LuxPlaybackEngineEvent) => void): () => void;
  destroy(): void;
}

/**
 * Owns one engine generation and prevents late browser/worker events from
 * reaching a controller after the source has been replaced or the page left.
 */
export class LuxPlaybackEngineHandle {
  private destroyed = false;
  private lastSnapshot = EMPTY_LUX_PLAYBACK_SNAPSHOT;
  private readonly subscriptions = new Set<() => void>();

  constructor(private readonly engine: LuxPlaybackEngine) {}

  get kind() {
    return this.engine.kind;
  }

  get element() {
    return this.engine.element;
  }

  get performance() {
    return this.engine.performance;
  }

  get error() {
    return this.engine.error;
  }

  get isDestroyed() {
    return this.destroyed;
  }

  setSource(source: LuxPlaybackSource) {
    if (this.destroyed) return Promise.reject(new Error("播放引擎已销毁"));
    return this.engine.setSource(source);
  }

  play() {
    if (this.destroyed) return Promise.reject(new Error("播放引擎已销毁"));
    return this.engine.play();
  }

  pause() {
    if (!this.destroyed) this.engine.pause();
  }

  seek(seconds: number) {
    if (!this.destroyed) this.engine.seek(seconds);
  }

  snapshot() {
    if (this.destroyed) return this.lastSnapshot;
    this.lastSnapshot = this.engine.snapshot();
    return this.lastSnapshot;
  }

  subscribe(listener: (event: LuxPlaybackEngineEvent) => void) {
    if (this.destroyed) return () => undefined;
    let active = true;
    const unsubscribeFromEngine = this.engine.subscribe((event) => {
      if (active && !this.destroyed) listener(event);
    });
    const unsubscribe = () => {
      if (!active) return;
      active = false;
      unsubscribeFromEngine();
      this.subscriptions.delete(unsubscribe);
    };
    this.subscriptions.add(unsubscribe);
    return unsubscribe;
  }

  destroy() {
    if (this.destroyed) return;
    this.destroyed = true;
    for (const unsubscribe of [...this.subscriptions]) unsubscribe();
    this.subscriptions.clear();
    this.engine.destroy();
  }
}
