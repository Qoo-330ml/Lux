import { LuxPlayerController } from "./player-controller";
import { LuxPlaybackEngineHandle, type LuxPlaybackEngine } from "./playback-engine";
import type { LuxPlaybackSource, LuxPlayerState } from "./types";

type LuxPlayerRuntimeListener = (state: LuxPlayerState) => void;

export class LuxPlayerRuntime {
  private readonly controller = new LuxPlayerController();
  private activeEngine: LuxPlaybackEngineHandle | null = null;
  private removeEngineSubscription: (() => void) | null = null;

  get state() {
    return this.controller.state;
  }

  get element() {
    return this.activeEngine?.element ?? null;
  }

  subscribe(listener: LuxPlayerRuntimeListener) {
    return this.controller.subscribe(listener);
  }

  async load(engine: LuxPlaybackEngine, source: LuxPlaybackSource) {
    this.disposeActiveEngine();
    this.controller.dispatch({ type: "LOAD", source });
    const handle = new LuxPlaybackEngineHandle(engine);
    this.activeEngine = handle;
    this.removeEngineSubscription = handle.subscribe((event) => {
      this.controller.dispatchEngineEvent(event);
    });
    try {
      await handle.setSource(source);
    } catch (cause) {
      if (this.activeEngine === handle) {
        this.controller.dispatchEngineEvent({
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
    this.disposeActiveEngine();
    this.controller.dispatch({ type: "RESET" });
  }

  private disposeActiveEngine() {
    this.removeEngineSubscription?.();
    this.removeEngineSubscription = null;
    this.activeEngine?.destroy();
    this.activeEngine = null;
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
