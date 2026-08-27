import {
  initialLuxPlayerState,
  reduceLuxPlayerState,
} from "./player-state";
import type {
  LuxPlaybackEngineEvent,
  LuxPlayerCommand,
  LuxPlayerEvent,
  LuxPlayerState,
} from "./types";

type LuxPlayerControllerListener = (state: LuxPlayerState) => void;

export class LuxPlayerController {
  private currentState = initialLuxPlayerState();
  private readonly listeners = new Set<LuxPlayerControllerListener>();

  get state() {
    return this.currentState;
  }

  subscribe(listener: LuxPlayerControllerListener) {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  dispatch(command: LuxPlayerCommand) {
    return this.apply(this.toEvent(command));
  }

  dispatchEngineEvent(event: LuxPlaybackEngineEvent) {
    const generation = this.currentState.generation;
    return this.apply(this.engineEventToPlayerEvent(event, generation));
  }

  private apply(event: LuxPlayerEvent) {
    const nextState = reduceLuxPlayerState(this.currentState, event);
    if (nextState === this.currentState) return this.currentState;
    this.currentState = nextState;
    this.listeners.forEach((listener) => listener(nextState));
    return nextState;
  }

  private toEvent(command: LuxPlayerCommand): LuxPlayerEvent {
    const generation = this.currentState.generation;
    switch (command.type) {
      case "LOAD":
        return { type: "LOAD", source: command.source } as const;
      case "PLAY":
        return { type: "PLAY", generation } as const;
      case "PAUSE":
        return { type: "PAUSE", generation, snapshot: command.snapshot } as const;
      case "WAITING":
        return { type: "WAITING", generation } as const;
      case "CAN_PLAY":
        return { type: "CAN_PLAY", generation, playing: command.playing } as const;
      case "SEEK":
        return { type: "SEEK_START", generation, position: command.position } as const;
      case "RESET":
        return { type: "RESET" } as const;
    }
  }

  private engineEventToPlayerEvent(
    event: LuxPlaybackEngineEvent,
    generation: number,
  ): LuxPlayerEvent {
    switch (event.type) {
      case "SOURCE_READY":
        return { type: "SOURCE_READY", generation, snapshot: event.snapshot };
      case "PLAYING":
        return { type: "PLAY", generation };
      case "PAUSED":
        return { type: "PAUSE", generation, snapshot: event.snapshot };
      case "WAITING":
        return { type: "WAITING", generation };
      case "CAN_PLAY":
        return { type: "CAN_PLAY", generation, playing: event.playing };
      case "SEEK_START":
        return { type: "SEEK_START", generation, position: event.position };
      case "SEEKED":
        return {
          type: "SEEKED",
          generation,
          snapshot: event.snapshot,
          playing: event.playing,
        };
      case "TIME_UPDATE":
        return { type: "TIME_UPDATE", generation, snapshot: event.snapshot };
      case "ENDED":
        return { type: "ENDED", generation, snapshot: event.snapshot };
      case "ERROR":
        return { type: "ERROR", generation, error: event.error };
    }
  }
}
