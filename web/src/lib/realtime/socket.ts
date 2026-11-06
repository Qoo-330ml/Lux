import {
  parseRealtimeFrame,
  type InvalidateFrame,
  type RealtimeConnectionState,
} from "./types";

/**
 * Realtime client for `GET /api/v1/ws` (ADR-054 "关停与重连语义").
 *
 * One instance owns exactly one connection at a time and never more than one
 * timer of each kind: a pending reconnect, the application ping interval and
 * the "connection opened but never said ready" deadline. Every timer is cleared
 * before a new one is armed, so a laptop lid closing while the tab is hidden
 * cannot accumulate work.
 */

/** ADR-054 D1/D6: one endpoint, and credentials travel as cookies, never in the URL. */
export const REALTIME_PATH = "/api/v1/ws";

/**
 * Backoff: `min(30s, 500ms * 2^n)` with +-20% jitter (ADR-054 D11).
 *
 * `n` counts consecutive failures since the last `ready` frame, so the first
 * retry after a drop waits 500ms and the sequence is 500ms, 1s, 2s, 4s, 8s,
 * 16s, 30s, 30s... The first retry is deliberately short because the common
 * cause is a proxy or process restart measured in seconds; the 30s base cap
 * bounds the reconnect storm the ADR calls out as risk 3.
 */
export const RECONNECT_BASE_DELAY_MS = 500;
export const RECONNECT_MAX_DELAY_MS = 30_000;

/**
 * Jitter amplitude. ADR-054 D11 fixes the band at 10%-20%; this client uses the
 * 20% the ADR states so the wait lands in [0.8x, 1.2x] of the base delay, which
 * spreads a fleet of tabs that dropped at the same instant (a server restart)
 * over 40% of the base interval instead of synchronising them again.
 */
export const RECONNECT_JITTER_RATIO = 0.2;

/**
 * ADR-054 D11: after `1000`/`1001` the client must wait at least 5s, because
 * the server is probably restarting and an immediate retry only burns a budget
 * slot. The backoff still grows underneath this floor.
 */
export const RECONNECT_AFTER_RESTART_MS = 5_000;

/**
 * A socket that completed the handshake but never delivered `ready` is a
 * failure (ADR-054 D11). The server writes `ready` immediately after the
 * upgrade, so 10s is two orders of magnitude of slack over a healthy handshake
 * while still being far below the 120s idle close: a half-open connection is
 * recycled long before either peer gives up.
 */
export const READY_TIMEOUT_MS = 10_000;

/**
 * Application ping cadence, 25s.
 *
 * ADR-054 "心跳与空闲" closes a connection after 120s without any inbound frame
 * (including protocol-level pongs), so the client must write something well
 * inside that window: four missed pings in a row is still under 120s. 25s also
 * matches the server's own protocol ping interval, which keeps the client's
 * keepalive traffic on the same rhythm as the heartbeat it is resetting.
 */
export const PING_INTERVAL_MS = 25_000;
export const PING_FRAME = '{"type":"ping"}';

/**
 * ADR-054 D11: a reverse-proxy failure and a 401 both surface as `1006` in the
 * browser, so every fifth consecutive abnormal close asks the API whether the
 * session is still valid before continuing to back off.
 */
export const ABNORMAL_CLOSE_PROBE_THRESHOLD = 5;

export const REALTIME_CLOSE_NORMAL = 1000;
export const REALTIME_CLOSE_GOING_AWAY = 1001;
export const REALTIME_CLOSE_ABNORMAL = 1006;
export const REALTIME_CLOSE_POLICY = 1008;

export type RealtimeSocketOptions = {
  /** The first `ready` of a connection; the only signal that resets the backoff. */
  onReady: () => void;
  onInvalidate: (frame: InvalidateFrame) => void;
  onStateChange: (state: RealtimeConnectionState) => void;
  /** A `1006` streak reached the probe threshold; the caller checks `GET /auth/me`. */
  onSessionCheck: () => void;
  /** `1008`: the session is gone, do not reconnect. */
  onSessionEnded: () => void;
};

type WebSocketConstructor = typeof WebSocket;

function webSocketConstructor(): WebSocketConstructor | undefined {
  const candidate = (globalThis as { WebSocket?: unknown }).WebSocket;
  return typeof candidate === "function" ? (candidate as WebSocketConstructor) : undefined;
}

function documentHidden(): boolean {
  return typeof document !== "undefined" && document.hidden === true;
}

/** `wss:` for an https page, `ws:` otherwise; host[:port] only, no query, no credentials. */
export function realtimeSocketUrl(): string {
  const { protocol, host } = globalThis.location;
  return `${protocol === "https:" ? "wss:" : "ws:"}//${host}${REALTIME_PATH}`;
}

/**
 * Pure delay formula, exported so the jitter band is testable without timers.
 * `attempt` is the value of `n` for this retry (0 for the first retry).
 */
export function reconnectDelay(attempt: number, random: () => number = Math.random): number {
  const exponent = Math.max(0, Math.min(attempt, 16));
  const base = Math.min(RECONNECT_MAX_DELAY_MS, RECONNECT_BASE_DELAY_MS * 2 ** exponent);
  const spread = (random() * 2 - 1) * RECONNECT_JITTER_RATIO;
  return Math.round(base * (1 + spread));
}

export class RealtimeSocket {
  private readonly options: RealtimeSocketOptions;
  private socket: WebSocket | null = null;
  private socketListeners: [string, EventListener][] = [];
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private readyTimer: ReturnType<typeof setTimeout> | null = null;
  private pingTimer: ReturnType<typeof setInterval> | null = null;
  private attempt = 0;
  private abnormalStreak = 0;
  private state: RealtimeConnectionState = "disconnected";
  private ready = false;
  private pendingRetry = false;
  private offline = typeof navigator !== "undefined" && navigator.onLine === false;
  private started = false;
  private stopped = false;

  constructor(options: RealtimeSocketOptions) {
    this.options = options;
  }

  start() {
    if (this.started) return;
    this.started = true;
    this.stopped = false;
    if (typeof document !== "undefined") {
      document.addEventListener("visibilitychange", this.handleVisibilityChange);
    }
    if (typeof window !== "undefined") {
      window.addEventListener("offline", this.handleOffline);
      window.addEventListener("online", this.handleOnline);
    }
    this.connect();
  }

  stop() {
    this.stopped = true;
    this.pendingRetry = false;
    this.clearReconnectTimer();
    this.teardownSocket(true);
    if (typeof document !== "undefined") {
      document.removeEventListener("visibilitychange", this.handleVisibilityChange);
    }
    if (typeof window !== "undefined") {
      window.removeEventListener("offline", this.handleOffline);
      window.removeEventListener("online", this.handleOnline);
    }
  }

  private connect() {
    if (this.stopped) return;
    this.clearReconnectTimer();
    const Constructor = webSocketConstructor();
    if (!Constructor) {
      // No transport at all: stay on the existing polling fallback instead of retrying.
      this.stopped = true;
      return;
    }
    if (this.suspended()) {
      this.pendingRetry = true;
      return;
    }
    this.pendingRetry = false;

    let socket: WebSocket;
    try {
      socket = new Constructor(realtimeSocketUrl());
    } catch {
      this.handleFailure(REALTIME_CLOSE_ABNORMAL);
      return;
    }

    const handleOpen: EventListener = () => this.handleOpen(socket);
    const handleMessage: EventListener = (event) => this.handleMessage(event as MessageEvent);
    const handleClose: EventListener = (event) => this.handleClose(socket, event as CloseEvent);
    const handleError: EventListener = () => this.handleError();
    socket.addEventListener("open", handleOpen);
    socket.addEventListener("message", handleMessage);
    socket.addEventListener("close", handleClose);
    socket.addEventListener("error", handleError);
    this.socket = socket;
    this.socketListeners = [
      ["open", handleOpen],
      ["message", handleMessage],
      ["close", handleClose],
      ["error", handleError],
    ];
  }

  private handleOpen(socket: WebSocket) {
    if (socket !== this.socket) return;
    this.ready = false;
    this.startPing();
    this.clearReadyTimer();
    this.readyTimer = setTimeout(() => {
      this.readyTimer = null;
      if (socket !== this.socket || this.ready) return;
      // Opened but never said ready: treat it as a failed attempt, not as a connection.
      this.failConnection(socket);
    }, READY_TIMEOUT_MS);
  }

  private handleMessage(event: MessageEvent) {
    const frame = parseRealtimeFrame(event.data);
    if (!frame) return;
    if (frame.type === "ready") {
      this.markReady();
      return;
    }
    this.options.onInvalidate(frame);
  }

  private markReady() {
    this.clearReadyTimer();
    if (this.ready) return;
    this.ready = true;
    // The only place the backoff counter is cleared (ADR-054 D11). The abnormal-close streak is
    // deliberately untouched: "5 consecutive 1006" counts closes, and a tab that reconnects,
    // receives `ready` and is dropped again by a broken proxy is exactly the case it detects.
    this.attempt = 0;
    this.setState("connected");
    this.options.onReady();
  }

  /**
   * Transport errors are ignored on purpose. Browsers follow every `error` with a `close` that
   * carries the code the reconnect policy needs, so acting here would schedule it twice.
   */
  private handleError() {}

  private handleClose(socket: WebSocket, event: CloseEvent) {
    if (socket !== this.socket) return;
    const code = typeof event?.code === "number" ? event.code : REALTIME_CLOSE_ABNORMAL;
    this.teardownSocket(false);
    this.setState("disconnected");
    this.handleFailure(code);
  }

  /** Drops a socket the client itself gives up on (the close event that follows is ignored). */
  private failConnection(socket: WebSocket) {
    if (socket !== this.socket) return;
    this.teardownSocket(true);
    this.setState("disconnected");
    this.handleFailure(REALTIME_CLOSE_ABNORMAL);
  }

  private handleFailure(code: number) {
    if (this.stopped) return;
    if (code === REALTIME_CLOSE_POLICY) {
      this.stop();
      this.options.onSessionEnded();
      return;
    }
    if (code === REALTIME_CLOSE_ABNORMAL) {
      this.abnormalStreak += 1;
      if (this.abnormalStreak >= ABNORMAL_CLOSE_PROBE_THRESHOLD) {
        this.abnormalStreak = 0;
        this.options.onSessionCheck();
      }
    } else {
      this.abnormalStreak = 0;
    }

    const delay = reconnectDelay(this.attempt);
    this.attempt += 1;
    const floor = code === REALTIME_CLOSE_NORMAL || code === REALTIME_CLOSE_GOING_AWAY
      ? RECONNECT_AFTER_RESTART_MS
      : 0;
    this.scheduleReconnect(Math.max(delay, floor));
  }

  private scheduleReconnect(delay: number) {
    this.clearReconnectTimer();
    if (this.stopped) return;
    if (this.suspended()) {
      // Hidden tab or offline network: remember the debt and retry on the next resume.
      this.pendingRetry = true;
      return;
    }
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.connect();
    }, delay);
  }

  private startPing() {
    this.stopPing();
    this.pingTimer = setInterval(() => this.sendPing(), PING_INTERVAL_MS);
  }

  private stopPing() {
    if (this.pingTimer !== null) {
      clearInterval(this.pingTimer);
      this.pingTimer = null;
    }
  }

  private sendPing() {
    const socket = this.socket;
    const Constructor = webSocketConstructor();
    if (!socket || !Constructor || socket.readyState !== Constructor.OPEN) return;
    try {
      socket.send(PING_FRAME);
    } catch {
      // The close that follows reports the failure; a thrown keepalive is not fatal here.
    }
  }

  private teardownSocket(closeSocket: boolean) {
    this.clearReadyTimer();
    this.stopPing();
    const socket = this.socket;
    this.socket = null;
    this.ready = false;
    for (const [type, listener] of this.socketListeners) {
      socket?.removeEventListener(type, listener);
    }
    this.socketListeners = [];
    if (closeSocket && socket) {
      const Constructor = webSocketConstructor();
      if (!Constructor || socket.readyState !== Constructor.CLOSED) {
        socket.close();
      }
    }
  }

  private suspended(): boolean {
    return this.offline || documentHidden();
  }

  private suspend() {
    this.clearReconnectTimer();
    if (this.socket === null) this.pendingRetry = true;
  }

  private resume() {
    if (this.stopped || this.socket !== null || !this.pendingRetry || this.suspended()) return;
    this.pendingRetry = false;
    this.connect();
  }

  private handleVisibilityChange = () => {
    if (this.stopped) return;
    if (documentHidden()) {
      this.suspend();
      return;
    }
    this.resume();
  };

  private handleOffline = () => {
    this.offline = true;
    if (!this.stopped) this.suspend();
  };

  private handleOnline = () => {
    this.offline = false;
    if (!this.stopped) this.resume();
  };

  private setState(next: RealtimeConnectionState) {
    if (this.state === next) return;
    this.state = next;
    this.options.onStateChange(next);
  }

  private clearReconnectTimer() {
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
  }

  private clearReadyTimer() {
    if (this.readyTimer !== null) {
      clearTimeout(this.readyTimer);
      this.readyTimer = null;
    }
  }
}
