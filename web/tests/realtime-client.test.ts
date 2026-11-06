// @vitest-environment jsdom

import { QueryClient } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { api, ApiError } from "../src/lib/api/client";
import {
  realtimeConnectionState,
  subscribeRealtime,
  type RealtimeHandlers,
} from "../src/lib/realtime/client";
import { FakeWebSocket, installFakeWebSocket } from "./support/fake-websocket";

const cleanups: Array<() => void> = [];

/** Subscribes to the shared hub and returns the handlers a test can assert on. */
function subscribe(client: QueryClient): Required<RealtimeHandlers> {
  const handlers: Required<RealtimeHandlers> = {
    onReady: vi.fn(),
    onInvalidate: vi.fn(),
    onStateChange: vi.fn(),
    onConnectionFailure: vi.fn(),
  };
  cleanups.push(subscribeRealtime(client, handlers));
  return handlers;
}

function lastSocket(): FakeWebSocket {
  const socket = FakeWebSocket.last();
  if (!socket) throw new Error("no realtime socket was created");
  return socket;
}

beforeEach(() => {
  installFakeWebSocket();
  vi.spyOn(Math, "random").mockReturnValue(0.5);
  vi.useFakeTimers();
});

afterEach(() => {
  for (const unsubscribe of cleanups.splice(0)) unsubscribe();
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

it("connects a subscriber that arrives after the session ended", () => {
  const client = new QueryClient();
  const first = subscribe(client);
  expect(FakeWebSocket.instances).toHaveLength(1);
  lastSocket().ready();
  expect(realtimeConnectionState()).toBe("connected");
  expect(first.onStateChange).toHaveBeenLastCalledWith("connected");

  // The server revokes the session. The shell may stay mounted for a moment, so the hub must not
  // keep a stopped socket around: a subscriber that arrives later would never get a connection.
  lastSocket().serverClose(1008);
  expect(realtimeConnectionState()).toBe("disconnected");

  const second = subscribe(client);
  expect(FakeWebSocket.instances).toHaveLength(2);
  lastSocket().ready();
  expect(realtimeConnectionState()).toBe("connected");
  expect(second.onStateChange).toHaveBeenLastCalledWith("connected");
});

it("broadcasts disconnected when the session ends on a live connection", async () => {
  const client = new QueryClient();
  const handlers = subscribe(client);
  let rejectProbe: ((reason: unknown) => void) | undefined;
  vi.spyOn(api, "me").mockImplementation(() => new Promise((_, reject) => {
    rejectProbe = reject;
  }));

  // Five abnormal closes end with a session probe.
  for (let attempt = 0; attempt < 4; attempt += 1) {
    lastSocket().ready();
    lastSocket().serverClose(1006);
    vi.advanceTimersByTime(500);
  }
  lastSocket().ready();
  lastSocket().serverClose(1006);
  // The probe is in flight while the client reconnects and comes back up.
  vi.advanceTimersByTime(500);
  lastSocket().ready();
  expect(realtimeConnectionState()).toBe("connected");

  rejectProbe?.(new ApiError("需要登录", { status: 401 }));
  await Promise.resolve();
  await Promise.resolve();

  // A 401 on a live connection still has to reach the subscribers through the normal channel.
  expect(realtimeConnectionState()).toBe("disconnected");
  expect(handlers.onStateChange).toHaveBeenLastCalledWith("disconnected");

  const sockets = FakeWebSocket.instances.length;
  vi.advanceTimersByTime(5 * 60_000);
  expect(FakeWebSocket.instances).toHaveLength(sockets);
});
