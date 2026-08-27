export type LuxPlayerStatus =
  | "IDLE"
  | "PREPARING"
  | "READY"
  | "PLAYING"
  | "PAUSED"
  | "BUFFERING"
  | "SEEKING"
  | "ENDED"
  | "FAILED";

export type LuxPlaybackEngineKind =
  | "native"
  | "hls"
  | "client-hevc"
  | "client-mkv"
  | "rust-wasm";

export type LuxPlaybackSource = {
  id: string;
  url: string;
  poster?: string | null;
};

export type LuxPlaybackSnapshot = {
  currentTime: number;
  duration: number | null;
  bufferedEnd: number;
  ended: boolean;
};

export type LuxPlaybackPerformance = {
  mediaDurationMs: number;
  processingDurationMs: number;
  speedX: number;
  realtime: boolean;
};

export type LuxPlayerErrorCode =
  | "UNSUPPORTED"
  | "RESOURCE_EXPIRED"
  | "ENGINE_FAILED"
  | "SERVER_PLAN_FAILED"
  | "NETWORK"
  | "ABORTED";

export type LuxPlayerError = {
  code: LuxPlayerErrorCode;
  message: string;
  recoverable: boolean;
  canFallback: boolean;
};

export type LuxPlayerEvent =
  | { type: "LOAD"; source: LuxPlaybackSource }
  | { type: "SOURCE_READY"; generation: number; snapshot: LuxPlaybackSnapshot }
  | { type: "PLAY"; generation: number }
  | { type: "PAUSE"; generation: number; snapshot?: LuxPlaybackSnapshot }
  | { type: "WAITING"; generation: number }
  | { type: "CAN_PLAY"; generation: number; playing: boolean }
  | { type: "SEEK_START"; generation: number; position: number }
  | {
      type: "SEEKED";
      generation: number;
      snapshot: LuxPlaybackSnapshot;
      playing: boolean;
    }
  | { type: "TIME_UPDATE"; generation: number; snapshot: LuxPlaybackSnapshot }
  | { type: "ENDED"; generation: number; snapshot: LuxPlaybackSnapshot }
  | { type: "ERROR"; generation: number; error: LuxPlayerError }
  | { type: "RESET" };

export type LuxPlayerCommand =
  | { type: "LOAD"; source: LuxPlaybackSource }
  | { type: "PLAY" }
  | { type: "PAUSE"; snapshot?: LuxPlaybackSnapshot }
  | { type: "WAITING" }
  | { type: "CAN_PLAY"; playing: boolean }
  | { type: "SEEK"; position: number }
  | { type: "RESET" };

export type LuxPlaybackEngineEvent =
  | { type: "SOURCE_READY"; snapshot: LuxPlaybackSnapshot }
  | { type: "PLAYING" }
  | { type: "PAUSED"; snapshot?: LuxPlaybackSnapshot }
  | { type: "WAITING" }
  | { type: "CAN_PLAY"; playing: boolean }
  | { type: "SEEK_START"; position: number }
  | { type: "SEEKED"; snapshot: LuxPlaybackSnapshot; playing: boolean }
  | { type: "TIME_UPDATE"; snapshot: LuxPlaybackSnapshot }
  | { type: "ENDED"; snapshot: LuxPlaybackSnapshot }
  | { type: "ERROR"; error: LuxPlayerError };

export type LuxPlayerState = {
  status: LuxPlayerStatus;
  generation: number;
  source: LuxPlaybackSource | null;
  snapshot: LuxPlaybackSnapshot;
  error: LuxPlayerError | null;
};

export const EMPTY_LUX_PLAYBACK_SNAPSHOT: LuxPlaybackSnapshot = {
  currentTime: 0,
  duration: null,
  bufferedEnd: 0,
  ended: false,
};
