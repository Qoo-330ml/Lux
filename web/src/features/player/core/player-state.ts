import {
  EMPTY_LUX_PLAYBACK_SNAPSHOT,
  type LuxPlaybackSnapshot,
  type LuxPlayerEvent,
  type LuxPlayerState,
} from "./types";

export type { LuxPlayerState } from "./types";

export function initialLuxPlayerState(): LuxPlayerState {
  return {
    status: "IDLE",
    generation: 0,
    source: null,
    snapshot: EMPTY_LUX_PLAYBACK_SNAPSHOT,
    error: null,
  };
}

export function reduceLuxPlayerState(
  state: LuxPlayerState,
  event: LuxPlayerEvent,
): LuxPlayerState {
  switch (event.type) {
    case "LOAD":
      return {
        status: "PREPARING",
        generation: state.generation + 1,
        source: event.source,
        snapshot: EMPTY_LUX_PLAYBACK_SNAPSHOT,
        error: null,
      };
    case "SOURCE_READY":
      if (!matchesGeneration(state, event.generation) || state.status !== "PREPARING") {
        return state;
      }
      return activeState("READY", state, event.snapshot);
    case "PLAY":
      if (!matchesGeneration(state, event.generation) || !canPlay(state.status)) {
        return state;
      }
      return activeState("PLAYING", state);
    case "PAUSE":
      if (!matchesGeneration(state, event.generation) || !canPause(state.status)) {
        return state;
      }
      return activeState("PAUSED", state, event.snapshot);
    case "WAITING":
      if (!matchesGeneration(state, event.generation) || !canBuffer(state.status)) {
        return state;
      }
      return activeState("BUFFERING", state);
    case "CAN_PLAY":
      if (!matchesGeneration(state, event.generation) || state.status !== "BUFFERING") {
        return state;
      }
      return activeState(event.playing ? "PLAYING" : "PAUSED", state);
    case "SEEK_START":
      if (!matchesGeneration(state, event.generation) || !canSeek(state.status)) {
        return state;
      }
      return activeState("SEEKING", state, {
        ...state.snapshot,
        currentTime: clampPosition(event.position, state.snapshot.duration),
        ended: false,
      });
    case "SEEKED":
      if (!matchesGeneration(state, event.generation) || state.status !== "SEEKING") {
        return state;
      }
      return activeState(event.playing ? "PLAYING" : "PAUSED", state, event.snapshot);
    case "TIME_UPDATE":
      if (!matchesGeneration(state, event.generation) || !isActive(state.status)) {
        return state;
      }
      return activeState(state.status, state, event.snapshot);
    case "ENDED":
      if (!matchesGeneration(state, event.generation) || !isActive(state.status)) {
        return state;
      }
      return activeState("ENDED", state, event.snapshot);
    case "ERROR":
      if (!matchesGeneration(state, event.generation) || !isActive(state.status)) {
        return state;
      }
      return {
        ...state,
        status: "FAILED",
        error: event.error,
      };
    case "RESET":
      return {
        status: "IDLE",
        generation: state.generation + 1,
        source: null,
        snapshot: EMPTY_LUX_PLAYBACK_SNAPSHOT,
        error: null,
      };
  }
}

function matchesGeneration(state: LuxPlayerState, generation: number) {
  return state.generation === generation;
}

function activeState(
  status: LuxPlayerState["status"],
  state: LuxPlayerState,
  snapshot = state.snapshot,
): LuxPlayerState {
  return {
    ...state,
    status,
    snapshot: normalizeSnapshot(snapshot),
    error: null,
  };
}

function normalizeSnapshot(snapshot: LuxPlaybackSnapshot): LuxPlaybackSnapshot {
  const duration = finiteOrNull(snapshot.duration);
  const currentTime = clampPosition(snapshot.currentTime, duration);
  const bufferedEnd = Math.max(currentTime, clampPosition(snapshot.bufferedEnd, duration));
  return {
    currentTime,
    duration,
    bufferedEnd,
    ended: snapshot.ended,
  };
}

function finiteOrNull(value: number | null) {
  return value !== null && Number.isFinite(value) && value >= 0 ? value : null;
}

function clampPosition(value: number, duration: number | null) {
  const position = Number.isFinite(value) && value >= 0 ? value : 0;
  return duration === null ? position : Math.min(position, duration);
}

function isActive(status: LuxPlayerState["status"]) {
  return status !== "IDLE" && status !== "FAILED";
}

function canPlay(status: LuxPlayerState["status"]) {
  return status === "READY" || status === "PAUSED" || status === "BUFFERING";
}

function canPause(status: LuxPlayerState["status"]) {
  return status === "READY" || status === "PLAYING" || status === "BUFFERING" || status === "SEEKING";
}

function canBuffer(status: LuxPlayerState["status"]) {
  return status === "READY" || status === "PLAYING" || status === "SEEKING";
}

function canSeek(status: LuxPlayerState["status"]) {
  return status === "READY" || status === "PLAYING" || status === "PAUSED" || status === "BUFFERING";
}
