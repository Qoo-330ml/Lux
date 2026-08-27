import { LuxPlayerController } from "./player-controller";
import { LuxPlaybackEngineHandle, type LuxPlaybackEngine } from "./playback-engine";
import type {
  LuxPlaybackEngineEvent,
  LuxPlaybackSource,
  LuxPlayerState,
} from "./types";

type LuxPlayerRuntimeListener = (state: LuxPlayerState) => void;
type LuxPlayerRuntimeEventListener = (event: LuxPlaybackEngineEvent) => void;

export class LuxPlayerRuntime {
  private readonly controller = new LuxPlayerController();
  private readonly eventListeners = new Set<LuxPlayerRuntimeEventListener>();
  private activeEngine: LuxPlaybackEngineHandle | null = null;
  private removeEngineSubscription: (() => void) | null = null;
  private operationGeneration = 0;

  get state() {
    return this.controller.state;
  }

  get element() {
    return this.activeEngine?.element ?? null;
  }

  subscribe(listener: LuxPlayerRuntimeListener) {
    return this.controller.subscribe(listener);
  }

  subscribeEvents(listener: LuxPlayerRuntimeEventListener) {
    this.eventListeners.add(listener);
    return () => this.eventListeners.delete(listener);
  }

  async load(engine: LuxPlaybackEngine, source: LuxPlaybackSource) {
    const operationGeneration = ++this.operationGeneration;
    this.disposeActiveEngine();
    this.controller.dispatch({ type: "LOAD", source });
    const handle = new LuxPlaybackEngineHandle(engine);
    this.activeEngine = handle;
    this.removeEngineSubscription = handle.subscribe((event) => this.dispatchEngineEvent(event));
    try {
      await handle.setSource(source);
      if (
        operationGeneration !== this.operationGeneration
        || this.activeEngine !== handle
        || handle.isDestroyed
      ) {
        handle.destroy();
        return this.state;
      }
    } catch (cause) {
      if (
        operationGeneration !== this.operationGeneration
        || this.activeEngine !== handle
        || handle.isDestroyed
      ) {
        return this.state;
      }
      if (this.activeEngine === handle) {
        this.dispatchEngineEvent({
          type: "ERROR",
          error: runtimeError(cause),
        });
      }
      throw cause;
    }
    return this.state;
  }

  play() {
    return this.activeEngine?.play() ?? Promise.reject(new Error("没有可播放的媒体引擎"));
  }

  pause() {
    this.activeEngine?.pause();
  }

  seek(seconds: number) {
    this.activeEngine?.seek(seconds);
  }

  destroy() {
    if (
      this.activeEngine === null
      && this.removeEngineSubscription === null
      && this.state.status === "IDLE"
    ) {
      return;
    }
    this.operationGeneration += 1;
    this.disposeActiveEngine();
    this.controller.dispatch({ type: "RESET" });
  }

  private disposeActiveEngine() {
    this.removeEngineSubscription?.();
    this.removeEngineSubscription = null;
    this.activeEngine?.destroy();
    this.activeEngine = null;
  }

  private dispatchEngineEvent(event: LuxPlaybackEngineEvent) {
    this.controller.dispatchEngineEvent(event);
    this.eventListeners.forEach((listener) => listener(event));
  }
}

function runtimeError(cause: unknown) {
  return {
    code: "ENGINE_FAILED" as const,
    message: cause instanceof Error ? cause.message : "媒体引擎加载失败",
    recoverable: true,
    canFallback: true,
  };
}
