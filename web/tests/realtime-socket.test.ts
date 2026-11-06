// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  PING_INTERVAL_MS,
  READY_TIMEOUT_MS,
  RECONNECT_BASE_DELAY_MS,
  RECONNECT_JITTER_RATIO,
  RECONNECT_MAX_DELAY_MS,
  RealtimeSocket,
  reconnectDelay,
  type RealtimeSocketOptions,
} from "../src/lib/realtime/socket";
import { parseRealtimeFrame } from "../src/lib/realtime/types";
import {
  FakeWebSocket,
  dispatchNetworkStatus,
  dispatchVisibilityChange,
  installFakeWebSocket,
  setDocumentHidden,
} from "./support/fake-websocket";

type Harness = {
  socket: RealtimeSocket;
  options: { [K in keyof RealtimeSocketOptions]: ReturnType<typeof vi.fn> };
  states: string[];
};

/** Every socket a test started, so a failing assertion cannot leak window listeners. */
const startedSockets: RealtimeSocket[] = [];

function createHarness(): Harness {
  const states: string[] = [];
  const options = {
    onReady: vi.fn(),
    onInvalidate: vi.fn(),
    onStateChange: vi.fn((state: string) => states.push(state)),
    onConnectionFailure: vi.fn(),
    onSessionCheck: vi.fn(),
    onSessionEnded: vi.fn(),
  };
  const socket = new RealtimeSocket(options);
  startedSockets.push(socket);
  return { socket, options, states };
}

/** Lets the client create a socket, complete the handshake and receive `ready`. */
function connect(harness: Harness, code = 1006) {
  const ws = FakeWebSocket.last();
  if (!ws) throw new Error("no socket was created");
  ws.ready();
  return { ws, close: (closeCode = code) => ws.serverClose(closeCode) };
}

let restoreVisibility: (() => void) | null = null;

beforeEach(() => {
  vi.useFakeTimers();
  installFakeWebSocket();
  vi.spyOn(Math, "random").mockReturnValue(0.5);
  restoreVisibility = null;
});

afterEach(() => {
  // A test that fails before its own `stop()` must not leave a socket listening to the window.
  for (const socket of startedSockets.splice(0)) socket.stop();
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  restoreVisibility?.();
  restoreVisibility = null;
});

describe("realtime socket url", () => {
  it("derives a credential-free ws url from the document location", () => {
    const { socket } = createHarness();
    socket.start();
    expect(FakeWebSocket.instances).toHaveLength(1);
    expect(FakeWebSocket.instances[0].url).toBe("ws://localhost:3000/api/v1/ws");
    expect(FakeWebSocket.instances[0].url).not.toContain("?");
    expect(FakeWebSocket.instances[0].url).not.toContain("token");
    socket.stop();
  });
});

describe("realtime frame parsing", () => {
  it("accepts ready and both channels while ignoring unknown fields", () => {
    expect(parseRealtimeFrame(JSON.stringify({ type: "ready", version: 1, seq: 7 })))
      .toEqual({ type: "ready", version: 1 });
    expect(parseRealtimeFrame(JSON.stringify({ type: "invalidate", channel: "user", scope: "home", extra: true })))
      .toEqual({ type: "invalidate", channel: "user", scope: "home", itemId: undefined });
    expect(parseRealtimeFrame(JSON.stringify({ type: "invalidate", channel: "admin", scope: "all" }))?.channel)
      .toBe("admin");
  });

  it("drops frames with an unknown channel, scope or item id", () => {
    expect(parseRealtimeFrame(JSON.stringify({ type: "invalidate", channel: "system", scope: "home" }))).toBeNull();
    expect(parseRealtimeFrame(JSON.stringify({ type: "invalidate", scope: "home" }))).toBeNull();
    expect(parseRealtimeFrame(JSON.stringify({ type: "invalidate", channel: "user" }))).toBeNull();
    expect(parseRealtimeFrame(JSON.stringify({ type: "invalidate", channel: "user", scope: 1 }))).toBeNull();
    expect(parseRealtimeFrame(JSON.stringify({ type: "invalidate", channel: "user", scope: "progress", itemId: 12 })))
      .toBeNull();
    expect(parseRealtimeFrame(JSON.stringify({ type: "invalidate", channel: "user", scope: "progress", itemId: " " })))
      .toBeNull();
    expect(parseRealtimeFrame(JSON.stringify({ type: "invalidate", channel: "user", scope: "progress", itemId: "x".repeat(257) })))
      .toBeNull();
    expect(parseRealtimeFrame(JSON.stringify({ type: "invalidate", channel: "user", scope: "progress", itemId: "x".repeat(256) }))?.itemId)
      .toHaveLength(256);
  });

  it("ignores unknown frame types and malformed payloads", () => {
    for (const payload of ["", "not-json", "null", "[]", JSON.stringify({ type: "pong" }), JSON.stringify({ scope: "home" })]) {
      expect(parseRealtimeFrame(payload)).toBeNull();
    }
    expect(parseRealtimeFrame(undefined)).toBeNull();
    expect(parseRealtimeFrame(new Uint8Array([1, 2, 3]))).toBeNull();
  });
});

describe("realtime backoff", () => {
  it("doubles from 500ms, caps the base at 30s and applies +-20% jitter", () => {
    vi.mocked(Math.random).mockReturnValue(0.5);
    expect([0, 1, 2, 3, 4, 5, 6, 7, 20].map((n) => reconnectDelay(n)))
      .toEqual([500, 1000, 2000, 4000, 8000, 16000, 30000, 30000, 30000]);

    vi.mocked(Math.random).mockReturnValue(1);
    expect(reconnectDelay(0)).toBe(Math.round(RECONNECT_BASE_DELAY_MS * (1 + RECONNECT_JITTER_RATIO)));
    expect(reconnectDelay(8)).toBe(Math.round(RECONNECT_MAX_DELAY_MS * (1 + RECONNECT_JITTER_RATIO)));

    vi.mocked(Math.random).mockReturnValue(0);
    expect(reconnectDelay(0)).toBe(Math.round(RECONNECT_BASE_DELAY_MS * (1 - RECONNECT_JITTER_RATIO)));
    expect(reconnectDelay(8)).toBe(Math.round(RECONNECT_MAX_DELAY_MS * (1 - RECONNECT_JITTER_RATIO)));
  });

  it("waits inside the jitter band before the first reconnect and doubles afterwards", () => {
    vi.mocked(Math.random).mockReturnValue(1);
    const harness = createHarness();
    harness.socket.start();
    connect(harness).close(1006);

    act(() => vi.advanceTimersByTime(599));
    expect(FakeWebSocket.instances).toHaveLength(1);
    act(() => vi.advanceTimersByTime(1));
    expect(FakeWebSocket.instances).toHaveLength(2);

    FakeWebSocket.last()?.serverClose(1006);
    act(() => vi.advanceTimersByTime(1199));
    expect(FakeWebSocket.instances).toHaveLength(2);
    act(() => vi.advanceTimersByTime(1));
    expect(FakeWebSocket.instances).toHaveLength(3);
  });

  it("resets the attempt counter only after the ready frame", () => {
    vi.mocked(Math.random).mockReturnValue(0.5);
    const harness = createHarness();
    harness.socket.start();
    // Three failures in a row grow the delay: 500, 1000, 2000.
    connect(harness).close(1006);
    act(() => vi.advanceTimersByTime(500));
    FakeWebSocket.last()?.serverClose(1006);
    act(() => vi.advanceTimersByTime(1000));
    FakeWebSocket.last()?.serverClose(1006);
    act(() => vi.advanceTimersByTime(2000));
    expect(FakeWebSocket.instances).toHaveLength(4);

    // A socket that opens but never sends `ready` is a failure: the counter keeps growing.
    harness.options.onReady.mockClear();
    FakeWebSocket.last()?.open();
    act(() => vi.advanceTimersByTime(READY_TIMEOUT_MS - 1));
    expect(harness.options.onReady).not.toHaveBeenCalled();
    act(() => vi.advanceTimersByTime(1));
    expect(harness.options.onReady).not.toHaveBeenCalled();
    act(() => vi.advanceTimersByTime(4000));
    expect(FakeWebSocket.instances).toHaveLength(5);

    // The frame that finally marks the connection up zeroes the counter.
    FakeWebSocket.last()?.ready();
    expect(harness.options.onReady).toHaveBeenCalledTimes(1);
    FakeWebSocket.last()?.serverClose(1006);
    act(() => vi.advanceTimersByTime(499));
    expect(FakeWebSocket.instances).toHaveLength(5);
    act(() => vi.advanceTimersByTime(1));
    expect(FakeWebSocket.instances).toHaveLength(6);
  });
});

describe("realtime failure reporting", () => {
  it("reports every retryable failure, including one that never reached ready", () => {
    const harness = createHarness();
    harness.socket.start();

    // The very first attempt is refused before the handshake completes. The connection state was
    // already "disconnected", so no state transition reports this on its own.
    FakeWebSocket.last()?.serverClose(1006);
    expect(harness.options.onConnectionFailure).toHaveBeenCalledTimes(1);
    expect(harness.states).toEqual([]);

    act(() => vi.advanceTimersByTime(500));
    // An attempt that opens but never says `ready` is a failure too.
    FakeWebSocket.last()?.open();
    act(() => vi.advanceTimersByTime(READY_TIMEOUT_MS));
    expect(harness.options.onConnectionFailure).toHaveBeenCalledTimes(2);

    // A 1008 ends the session instead of retrying, so it is not a "connection failure".
    act(() => vi.advanceTimersByTime(60_000));
    connect(harness).close(1008);
    expect(harness.options.onConnectionFailure).toHaveBeenCalledTimes(2);
    expect(harness.options.onSessionEnded).toHaveBeenCalledTimes(1);
  });

  it("does not report a failure for a connection that never opened and was stopped", () => {
    const harness = createHarness();
    harness.socket.start();
    harness.socket.stop();
    expect(harness.options.onConnectionFailure).not.toHaveBeenCalled();
  });
});

describe("realtime connection state", () => {
  it("stays disconnected until the ready frame and reports the transition once", () => {
    const harness = createHarness();
    harness.socket.start();
    expect(harness.states).toEqual([]);
    FakeWebSocket.last()?.open();
    expect(harness.states).toEqual([]);
    expect(harness.options.onReady).not.toHaveBeenCalled();

    FakeWebSocket.last()?.message({ type: "ready", version: 1 });
    expect(harness.states).toEqual(["connected"]);
    expect(harness.options.onReady).toHaveBeenCalledTimes(1);

    // A second ready frame (the server sends one per connection) is not a new open.
    FakeWebSocket.last()?.message({ type: "ready", version: 1 });
    expect(harness.options.onReady).toHaveBeenCalledTimes(1);

    FakeWebSocket.last()?.serverClose(1006);
    expect(harness.states).toEqual(["connected", "disconnected"]);
    harness.socket.stop();
  });

  it("routes invalidate frames and ignores unknown ones", () => {
    const harness = createHarness();
    harness.socket.start();
    connect(harness);
    FakeWebSocket.last()?.message({ type: "invalidate", channel: "user", scope: "home" });
    FakeWebSocket.last()?.message({ type: "pong" });
    FakeWebSocket.last()?.message("not-json");
    FakeWebSocket.last()?.message({ type: "invalidate", channel: "admin", scope: "jobs", itemId: "job-1" });
    expect(harness.options.onInvalidate.mock.calls.map(([frame]) => frame)).toEqual([
      { type: "invalidate", channel: "user", scope: "home", itemId: undefined },
      { type: "invalidate", channel: "admin", scope: "jobs", itemId: "job-1" },
    ]);
    harness.socket.stop();
  });
});

describe("realtime ping", () => {
  it("writes one application ping per interval and stops on close", () => {
    const harness = createHarness();
    harness.socket.start();
    const ws = FakeWebSocket.last();
    act(() => vi.advanceTimersByTime(PING_INTERVAL_MS * 3));
    expect(ws?.sent).toEqual([]);

    ws?.open();
    ws?.message({ type: "ready", version: 1 });
    act(() => vi.advanceTimersByTime(PING_INTERVAL_MS - 1));
    expect(ws?.sent).toEqual([]);
    act(() => vi.advanceTimersByTime(1));
    expect(ws?.sent).toEqual(['{"type":"ping"}']);
    act(() => vi.advanceTimersByTime(PING_INTERVAL_MS * 2));
    expect(ws?.sent).toEqual(['{"type":"ping"}', '{"type":"ping"}', '{"type":"ping"}']);

    ws?.serverClose(1006);
    const sentBeforeClose = ws?.sent.length ?? 0;
    act(() => vi.advanceTimersByTime(PING_INTERVAL_MS * 5));
    expect(ws?.sent).toHaveLength(sentBeforeClose);
    harness.socket.stop();
  });

  it("keeps the ping interval shorter than the server idle timeout", () => {
    // ADR-054: the server closes after 120s without any inbound frame, four pings fit in it.
    expect(PING_INTERVAL_MS).toBeGreaterThanOrEqual(25_000);
    expect(PING_INTERVAL_MS).toBeLessThanOrEqual(30_000);
    expect(PING_INTERVAL_MS * 4).toBeLessThanOrEqual(120_000);
  });
});

describe("realtime close codes", () => {
  it("never reconnects after 1008 and reports the ended session", () => {
    const harness = createHarness();
    harness.socket.start();
    connect(harness).close(1008);
    expect(harness.options.onSessionEnded).toHaveBeenCalledTimes(1);
    expect(harness.options.onSessionCheck).not.toHaveBeenCalled();
    act(() => vi.advanceTimersByTime(10 * 60_000));
    expect(FakeWebSocket.instances).toHaveLength(1);
  });

  it.each([1001, 1000])("waits at least 5s before reconnecting after %i", (code) => {
    vi.mocked(Math.random).mockReturnValue(0);
    const harness = createHarness();
    harness.socket.start();
    connect(harness).close(code);
    act(() => vi.advanceTimersByTime(4_999));
    expect(FakeWebSocket.instances).toHaveLength(1);
    act(() => vi.advanceTimersByTime(1));
    expect(FakeWebSocket.instances).toHaveLength(2);
    harness.socket.stop();
  });

  it("probes the session once after five consecutive 1006 closes", () => {
    const harness = createHarness();
    harness.socket.start();
    const abnormalCloses = (count: number) => {
      for (let index = 0; index < count; index += 1) {
        connect(harness).close(1006);
        act(() => vi.advanceTimersByTime(60_000));
      }
    };

    abnormalCloses(4);
    expect(harness.options.onSessionCheck).not.toHaveBeenCalled();
    abnormalCloses(1);
    expect(harness.options.onSessionCheck).toHaveBeenCalledTimes(1);

    // The counter restarts after the probe, so the next one only happens at the next streak.
    abnormalCloses(4);
    expect(harness.options.onSessionCheck).toHaveBeenCalledTimes(1);
    abnormalCloses(1);
    expect(harness.options.onSessionCheck).toHaveBeenCalledTimes(2);
    harness.socket.stop();
  });

  it.each([1003, 1009])("retries after %i, a code this client can never provoke", (code) => {
    // 1003 (binary) and 1009 (>4 KiB text) are the server's reactions to inbound frames the Web
    // client never sends: it only writes `{"type":"ping"}`. Treating them like any other retryable
    // close keeps the contract single-sourced - only 1008 ends the session (ADR-054 D11).
    vi.mocked(Math.random).mockReturnValue(0.5);
    const harness = createHarness();
    harness.socket.start();
    connect(harness).close(code);

    expect(harness.options.onSessionEnded).not.toHaveBeenCalled();
    act(() => vi.advanceTimersByTime(499));
    expect(FakeWebSocket.instances).toHaveLength(1);
    act(() => vi.advanceTimersByTime(1));
    expect(FakeWebSocket.instances).toHaveLength(2);
    harness.socket.stop();
  });

  it("does not count a clean session reset as an abnormal close", () => {
    const harness = createHarness();
    harness.socket.start();
    for (let attempt = 0; attempt < 6; attempt += 1) {
      connect(harness).close(1001);
      act(() => vi.advanceTimersByTime(5_000));
    }
    expect(harness.options.onSessionCheck).not.toHaveBeenCalled();
    harness.socket.stop();
  });
});

describe("realtime visibility and network", () => {
  it("pauses reconnects while the tab is hidden and retries once when it becomes visible", () => {
    const harness = createHarness();
    harness.socket.start();
    const ws = FakeWebSocket.last();
    restoreVisibility = setDocumentHidden(true);

    ws?.serverClose(1006);
    act(() => vi.advanceTimersByTime(5 * 60_000));
    expect(FakeWebSocket.instances).toHaveLength(1);

    restoreVisibility();
    restoreVisibility = null;
    act(() => dispatchVisibilityChange());
    expect(FakeWebSocket.instances).toHaveLength(2);
    harness.socket.stop();
  });

  it("keeps an open connection while hidden and does not duplicate the retry", () => {
    const harness = createHarness();
    harness.socket.start();
    connect(harness);
    restoreVisibility = setDocumentHidden(true);
    act(() => dispatchVisibilityChange());
    act(() => dispatchVisibilityChange());
    expect(FakeWebSocket.instances).toHaveLength(1);
    expect(FakeWebSocket.instances[0].closeCalls).toHaveLength(0);
    restoreVisibility();
    restoreVisibility = null;
    harness.socket.stop();
  });

  it("rebuilds an open socket when the network comes back", () => {
    const harness = createHarness();
    harness.socket.start();
    const stale = FakeWebSocket.last();
    stale?.ready();
    expect(harness.states).toEqual(["connected"]);

    // A Wi-Fi switch or a closed lid can leave a half-open socket that never fires `close`: the
    // browser keeps reporting it OPEN while nothing can arrive.
    act(() => dispatchNetworkStatus(false));
    expect(stale?.closeCalls).toHaveLength(0);
    expect(FakeWebSocket.instances).toHaveLength(1);

    act(() => dispatchNetworkStatus(true));
    expect(stale?.closeCalls).toHaveLength(1);
    expect(FakeWebSocket.instances).toHaveLength(2);
    expect(harness.states).toEqual(["connected", "disconnected"]);
    // The network was gone, so the user channel has to catch up once the retry is ready (D15①).
    expect(harness.options.onConnectionFailure).toHaveBeenCalledTimes(1);

    FakeWebSocket.last()?.ready();
    expect(harness.states).toEqual(["connected", "disconnected", "connected"]);
    expect(harness.options.onReady).toHaveBeenCalledTimes(2);
    harness.socket.stop();
  });

  it("pauses reconnects while offline and retries once when back online", () => {
    const harness = createHarness();
    harness.socket.start();
    const ws = FakeWebSocket.last();
    act(() => dispatchNetworkStatus(false));

    ws?.serverClose(1006);
    act(() => vi.advanceTimersByTime(5 * 60_000));
    expect(FakeWebSocket.instances).toHaveLength(1);

    act(() => dispatchNetworkStatus(true));
    expect(FakeWebSocket.instances).toHaveLength(2);
    harness.socket.stop();
  });

  it("does not accumulate timers across failed attempts", () => {
    const harness = createHarness();
    harness.socket.start();
    for (let attempt = 0; attempt < 6; attempt += 1) {
      connect(harness).close(1006);
      expect(vi.getTimerCount()).toBeLessThanOrEqual(1);
      act(() => vi.advanceTimersByTime(60_000));
    }
    harness.socket.stop();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("stops everything on stop(), including a scheduled reconnect", () => {
    const harness = createHarness();
    harness.socket.start();
    connect(harness).close(1006);
    harness.socket.stop();
    act(() => vi.advanceTimersByTime(10 * 60_000));
    expect(FakeWebSocket.instances).toHaveLength(1);
    expect(harness.states).toEqual(["connected", "disconnected"]);
  });

  it("closes the live socket when stop() runs while connected", () => {
    const harness = createHarness();
    harness.socket.start();
    connect(harness);
    expect(FakeWebSocket.instances[0].closeCalls).toHaveLength(0);
    harness.socket.stop();
    expect(FakeWebSocket.instances[0].closeCalls).toHaveLength(1);
  });
});

describe("realtime transport availability", () => {
  it("keeps the polling fallback when WebSocket is unavailable", () => {
    vi.stubGlobal("WebSocket", undefined);
    const harness = createHarness();
    harness.socket.start();
    expect(FakeWebSocket.instances).toHaveLength(0);
    expect(harness.states).toEqual([]);
    harness.socket.stop();
  });
});

/** react-test-renderer-style act() is not needed for plain class unit tests. */
function act(run: () => void) {
  run();
}
