import { api, ApiError } from "../api/client";
import { RealtimeSocket } from "./socket";
import type { InvalidateFrame, RealtimeConnectionState } from "./types";

/**
 * Page-wide owner of the single `/api/v1/ws` connection.
 *
 * Both the user channel (`useUserEvents` in `LuxShell`) and the admin channel
 * (`useAdminEvents` in `AdminLayout`) subscribe here instead of opening their own
 * socket: ADR-054 decision 1 puts both channels on one connection, and the ADR's
 * consequences section requires an admin tab to drop from two streams to one.
 * The connection exists only while at least one hook is mounted.
 */

export type RealtimeHandlers = {
  /** The first `ready` of a connection, i.e. what "open" used to mean for SSE. */
  onReady: () => void;
  onInvalidate: (frame: InvalidateFrame) => void;
  onStateChange?: (state: RealtimeConnectionState) => void;
  /** 1008, or a 1006 streak plus a `401` from `GET /api/v1/auth/me`. */
  onSessionEnded?: () => void;
};

let socket: RealtimeSocket | null = null;
let connectionState: RealtimeConnectionState = "disconnected";
const subscribers = new Set<RealtimeHandlers>();

/** Current state, for a hook that mounts while a connection already exists. */
export function realtimeConnectionState(): RealtimeConnectionState {
  return connectionState;
}

function createSocket(): RealtimeSocket {
  return new RealtimeSocket({
    onReady: () => {
      for (const subscriber of [...subscribers]) subscriber.onReady();
    },
    onInvalidate: (frame) => {
      for (const subscriber of [...subscribers]) subscriber.onInvalidate(frame);
    },
    onStateChange: (state) => {
      connectionState = state;
      for (const subscriber of [...subscribers]) subscriber.onStateChange?.(state);
    },
    onSessionCheck: () => {
      void probeSession();
    },
    onSessionEnded: () => {
      endSession();
    },
  });
}

/**
 * ADR-054 D11: a proxy failure and a revoked session both look like `1006`, so every fifth
 * abnormal close asks the API. Only an explicit `401` ends the session; anything else (500,
 * network error) keeps the backoff running and leaves the UI alone.
 */
async function probeSession() {
  const active = socket;
  try {
    await api.me();
  } catch (error) {
    if (!(error instanceof ApiError) || error.status !== 401) return;
    if (active !== socket) return;
    endSession();
  }
}

function endSession() {
  socket?.stop();
  connectionState = "disconnected";
  for (const subscriber of [...subscribers]) subscriber.onSessionEnded?.();
}

/**
 * Registers a listener for the shared connection and returns the unsubscribe function.
 * The socket is opened by the first subscriber and closed by the last one.
 */
export function subscribeRealtime(handlers: RealtimeHandlers): () => void {
  subscribers.add(handlers);
  if (!socket) {
    socket = createSocket();
    socket.start();
  }
  return () => {
    subscribers.delete(handlers);
    if (subscribers.size > 0) return;
    socket?.stop();
    socket = null;
    connectionState = "disconnected";
  };
}
