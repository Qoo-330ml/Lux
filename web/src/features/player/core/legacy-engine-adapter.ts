import type { PlaybackEngine, PlaybackEngineKind } from "../playback-engine";
import type {
  LuxPlaybackEngine,
  LuxPlaybackEngineEvent,
} from "./playback-engine";
import type {
  LuxPlaybackEngineKind,
  LuxPlaybackSource,
  LuxPlaybackSnapshot,
  LuxPlayerError,
} from "./types";

type MediaEventName = keyof HTMLMediaElementEventMap;

export class LegacyPlaybackEngineAdapter implements LuxPlaybackEngine {
  readonly kind: LuxPlaybackEngineKind;
  private destroyed = false;
  private readonly subscriptions = new Set<() => void>();

  constructor(
    private readonly engine: PlaybackEngine,
    kind?: LuxPlaybackEngineKind,
  ) {
    this.kind = kind ?? mapEngineKind(engine.kind);
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

  setSource(source: LuxPlaybackSource) {
    return this.engine.setSource(source.url, source.poster);
  }

  play() {
    return this.engine.play();
  }

  pause() {
    this.engine.pause();
  }

  seek(seconds: number) {
    this.engine.seek(seconds);
  }

  snapshot(): LuxPlaybackSnapshot {
    const snapshot = this.engine.snapshot();
    return {
      currentTime: snapshot.currentTime,
      duration: snapshot.duration,
      bufferedEnd: bufferedEnd(this.element, snapshot.currentTime),
      ended: snapshot.ended,
    };
  }

  subscribe(listener: (event: LuxPlaybackEngineEvent) => void) {
    if (this.destroyed) return () => undefined;
    const subscriptions: Array<() => void> = [];
    const on = (
      eventName: MediaEventName,
      createEvent: () => LuxPlaybackEngineEvent,
    ) => {
      const handler = () => {
        if (!this.destroyed) listener(createEvent());
      };
      this.element.addEventListener(eventName, handler);
      subscriptions.push(() => this.element.removeEventListener(eventName, handler));
    };

    on("loadedmetadata", () => ({ type: "SOURCE_READY", snapshot: this.snapshot() }));
    on("play", () => ({ type: "PLAYING" }));
    on("pause", () => ({ type: "PAUSED", snapshot: this.snapshot() }));
    on("waiting", () => ({ type: "WAITING" }));
    on("canplay", () => ({ type: "CAN_PLAY", playing: !this.element.paused }));
    on("seeking", () => ({ type: "SEEK_START", position: this.element.currentTime }));
    on("seeked", () => ({ type: "SEEKED", snapshot: this.snapshot(), playing: !this.element.paused }));
    on("timeupdate", () => ({ type: "TIME_UPDATE", snapshot: this.snapshot() }));
    on("ended", () => ({ type: "ENDED", snapshot: this.snapshot() }));
    on("error", () => ({ type: "ERROR", error: playbackError(this.engine.error, this.element.error) }));

    let active = true;
    const unsubscribe = () => {
      if (!active) return;
      active = false;
      subscriptions.splice(0).forEach((remove) => remove());
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

function mapEngineKind(kind: PlaybackEngineKind): LuxPlaybackEngineKind {
  return kind;
}

function bufferedEnd(video: HTMLVideoElement, currentTime: number) {
  if (video.buffered.length === 0) return currentTime;
  return video.buffered.end(video.buffered.length - 1);
}

function playbackError(engineError: Error | null, mediaError: MediaError | null): LuxPlayerError {
  const network = mediaError?.code === 2;
  return {
    code: network ? "NETWORK" : "ENGINE_FAILED",
    message: engineError?.message ?? "媒体引擎无法播放该媒体源",
    recoverable: true,
    canFallback: true,
  };
}
