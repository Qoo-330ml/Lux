import type { QueryClient } from "@tanstack/react-query";
import { api, ApiError } from "../api/client";
import { clearSessionCaches } from "../api/session";
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
  /** A retryable attempt failed; unlike `onStateChange` this also fires for the first attempt. */
  onConnectionFailure?: () => void;
};

let socket: RealtimeSocket | null = null;
let connectionState: RealtimeConnectionState = "disconnected";
/** The page-wide cache the subscribers share; cleared once when the session ends. */
let sessionQueryClient: QueryClient | null = null;
const subscribers = new Set<RealtimeHandlers>();

/** Current state, for a hook that mounts while a connection already exists. */
export function realtimeConnectionState(): RealtimeConnectionState {
  return connectionState;
}

/**
 * The only writer of `connectionState`: every change (from the socket or from the hub itself)
 * reaches the subscribers through the same path.
 */
function setConnectionState(next: RealtimeConnectionState) {
  if (connectionState === next) return;
  connectionState = next;
  for (const subscriber of [...subscribers]) subscriber.onStateChange?.(next);
}

function createSocket(): RealtimeSocket {
  return new RealtimeSocket({
    onReady: () => {
      for (const subscriber of [...subscribers]) subscriber.onReady();
    },
    onInvalidate: (frame) => {
      for (const subscriber of [...subscribers]) subscriber.onInvalidate(frame);
    },
    onStateChange: setConnectionState,
    onConnectionFailure: () => {
      for (const subscriber of [...subscribers]) subscriber.onConnectionFailure?.();
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

/**
 * The session is gone: stop pushing, tell every subscriber, and drop the caches the way an
 * explicit logout does.
 *
 * The stopped socket is dropped here rather than parked: `start()` on a stopped socket returns
 * immediately, so keeping it would leave the next subscriber (a re-mounted shell, a second tab's
 * admin layout) attached to a connection that can never come back. The invariant is "a subscriber
 * always has a live socket, or the hub has told it the channel is down"; the state broadcast below
 * is what makes the second half true, and a later subscriber gets a fresh socket.
 *
 * The caches are cleared because the `me` query is re-created by its observer in `App` and answers
 * 401 (which is what lands on the login page); without the clear a second account on the same tab
 * would still read the first account's home/favorites/history entries for the rest of their
 * `staleTime`.
 */
function endSession() {
  const ending = socket;
  socket = null;
  ending?.stop();
  setConnectionState("disconnected");
  if (sessionQueryClient) clearSessionCaches(sessionQueryClient);
}

/**
 * Registers a listener for the shared connection and returns the unsubscribe function.
 * The socket is opened by the first subscriber and closed by the last one.
 */
export function subscribeRealtime(queryClient: QueryClient, handlers: RealtimeHandlers): () => void {
  subscribers.add(handlers);
  sessionQueryClient = queryClient;
  if (!socket) {
    socket = createSocket();
    socket.start();
  }
  return () => {
    subscribers.delete(handlers);
    if (subscribers.size > 0) return;
    socket?.stop();
    socket = null;
    setConnectionState("disconnected");
  };
}
