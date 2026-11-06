// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { LuxShell } from "../src/components/layout/LuxShell";
import { useRealtimeConnectionState } from "../src/features/activity/useUserEvents";
import { api, ApiError } from "../src/lib/api/client";
import { queryKeys } from "../src/lib/api/query-keys";
import { FakeWebSocket, installFakeWebSocket } from "./support/fake-websocket";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

function ConnectionProbe() {
  return <output>{useRealtimeConnectionState()}</output>;
}

let container: HTMLDivElement;
let root: Root;
let client: QueryClient;

beforeEach(() => {
  installFakeWebSocket();
  // A constant jitter factor of 1.0 keeps the backoff steps exact under fake timers.
  vi.spyOn(Math, "random").mockReturnValue(0.5);
  vi.useFakeTimers();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
});

afterEach(() => {
  act(() => root.unmount());
  client.clear();
  container.remove();
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function renderShell() {
  act(() => root.render(
    <QueryClientProvider client={client}>
      <MemoryRouter><Routes>
        <Route element={<LuxShell user={{ id: "viewer", usernameNormalized: "viewer" }} />}>
          <Route index element={<ConnectionProbe />} />
        </Route>
      </Routes></MemoryRouter>
    </QueryClientProvider>,
  ));
}

function socket(index = -1): FakeWebSocket {
  const ws = FakeWebSocket.instances.at(index);
  if (!ws) throw new Error("no realtime socket was created");
  return ws;
}

async function advance(ms: number) {
  await act(async () => { await vi.advanceTimersByTimeAsync(ms); });
}

/** Completes the handshake of the newest socket the way the server does. */
function markReady(index = -1) {
  act(() => socket(index).ready());
}

function connectionText() {
  return container.querySelector("output")?.textContent;
}

it("opens one shared websocket and reports the connection only after the ready frame", async () => {
  renderShell();
  expect(connectionText()).toBe("disconnected");
  expect(FakeWebSocket.instances).toHaveLength(1);
  expect(socket().url).toBe("ws://localhost:3000/api/v1/ws");

  // A completed handshake without `ready` is not a usable connection (ADR-054 D11).
  act(() => socket().open());
  expect(connectionText()).toBe("disconnected");
  expect(socket().sent).toEqual([]);

  act(() => socket().message({ type: "ready", version: 1 }));
  expect(connectionText()).toBe("connected");

  act(() => socket().serverClose(1006));
  expect(connectionText()).toBe("disconnected");

  await advance(500);
  expect(FakeWebSocket.instances).toHaveLength(2);
  markReady();
  expect(connectionText()).toBe("connected");
  expect(FakeWebSocket.instances).toHaveLength(2);

  act(() => root.unmount());
  expect(socket().closeCalls).toHaveLength(1);
});

it("keeps the polling fallback when WebSocket is unavailable", () => {
  vi.stubGlobal("WebSocket", undefined);
  renderShell();
  expect(connectionText()).toBe("disconnected");
  expect(FakeWebSocket.instances).toHaveLength(0);
});

const mediaKeys = [["home", "carousel"], ["libraries"], ["library", "library-1"],
  ["item", "item-1"], ["item-images", "item-1"], ["playback", "item-1"],
  ["children", "series-1", "EPISODE"], ["favorites"], ["collection", "collection-1"]];

it("does not invalidate loaded media caches on the first ready and does it once after a drop", async () => {
  for (const key of mediaKeys) client.setQueryData(key, { value: "fresh" });
  renderShell();
  markReady();
  for (const key of mediaKeys) expect(client.getQueryState(key)?.isInvalidated).toBe(false);

  act(() => socket().serverClose(1006));
  await advance(500);
  const invalidate = vi.spyOn(client, "invalidateQueries");
  markReady();
  for (const key of mediaKeys) expect(client.getQueryState(key)?.isInvalidated).toBe(true);

  // `refreshQueries(["home"])` walks the cached home queries, so the carousel proves the sweep ran
  // exactly once instead of once per ready frame.
  const sweeps = invalidate.mock.calls.filter(
    ([options]) => JSON.stringify((options as { queryKey: unknown }).queryKey) === '["home","carousel"]',
  );
  expect(sweeps).toHaveLength(1);

  // A repeated ready frame on the same connection is not another open.
  const callsAfterReconnect = invalidate.mock.calls.length;
  act(() => socket().message({ type: "ready", version: 1 }));
  expect(invalidate.mock.calls).toHaveLength(callsAfterReconnect);
});

it.each(["home", "item", "library", "favorites"])("refreshes media caches for a %s event without touching player sessions", (scope) => {
  for (const key of mediaKeys) client.setQueryData(key, { value: "old" });
  const playerKey = ["web-playback-session", "item-1"];
  client.setQueryData(playerKey, { sessionId: "active-session" });
  renderShell();
  markReady();
  act(() => socket().message({ type: "invalidate", channel: "user", scope }));
  for (const key of mediaKeys) expect(client.getQueryState(key)?.isInvalidated).toBe(true);
  expect(client.getQueryState(playerKey)?.isInvalidated).toBe(false);
});

it("widens a library event to the whole library prefix and does not refetch in the background", async () => {
  client.setQueryData(["library", "library-1"], { value: "old" });
  client.setQueryData(["library", "library-2"], { value: "other" });
  const firstKey = ["library", "library-1"];
  let refetchCount = 0;
  client.setQueryDefaults(firstKey, { queryFn: async () => { refetchCount += 1; return { value: "new" }; } });
  renderShell();
  markReady();
  // ADR-054 D14: the server never sends `libraryId`, so the per-library narrowing is gone.
  act(() => socket().message({ type: "invalidate", channel: "user", scope: "library" }));
  await act(async () => { await Promise.resolve(); });
  expect(client.getQueryState(firstKey)?.isInvalidated).toBe(true);
  expect(client.getQueryState(["library", "library-2"])?.isInvalidated).toBe(true);
  expect(refetchCount).toBe(0);
});

it("ignores malformed, foreign and unknown invalidations", () => {
  client.setQueryData(["item", "item-1"], { title: "old" });
  renderShell();
  markReady();
  for (const payload of [
    null,
    {},
    { scope: "unknown" },
    { type: "invalidate", channel: "user", scope: "item", itemId: 12 },
    // A frame for the admin channel must not reach the user hook.
    { type: "invalidate", channel: "admin", scope: "jobs" },
    { type: "invalidate", channel: "user", scope: "item", itemId: "x".repeat(257) },
  ]) {
    act(() => socket().message(payload));
  }
  act(() => socket().message("not-json"));
  expect(client.getQueryState(["item", "item-1"])?.isInvalidated).toBe(false);
});

it("limits a targeted item event to that item and related list caches", () => {
  client.setQueryData(["item", "item-1"], { title: "old" });
  client.setQueryData(["item", "item-2"], { title: "other" });
  renderShell();
  markReady();
  act(() => socket().message({ type: "invalidate", channel: "user", scope: "item", itemId: "item-1" }));
  expect(client.getQueryState(["item", "item-1"])?.isInvalidated).toBe(true);
  expect(client.getQueryState(["item", "item-2"])?.isInvalidated).toBe(false);
});

const progressUntouchedKeys = [
  ["home"], ["home", "carousel"], ["home", "latest-libraries", ["library-1"]], ["libraries"],
  ["library", "library-1"], ["item", "item-1"], ["item-images", "item-1"],
  ["children", "series-1", "EPISODE"], ["favorites"], ["collection", "collection-1"],
  ["person", "person-1"], ["playback", "item-2"],
];

it("narrows a progress event to continue-watching and the playing item's playback state", () => {
  client.setQueryData(["home", "continue-watching"], { value: "old" });
  client.setQueryData(["playback", "item-1"], { value: "old" });
  for (const key of progressUntouchedKeys) client.setQueryData(key, { value: "old" });
  renderShell();
  markReady();
  act(() => socket().message({ type: "invalidate", channel: "user", scope: "progress", itemId: "item-1" }));
  expect(client.getQueryState(["home", "continue-watching"])?.isInvalidated).toBe(true);
  expect(client.getQueryState(["playback", "item-1"])?.isInvalidated).toBe(true);
  for (const key of progressUntouchedKeys) expect(client.getQueryState(key)?.isInvalidated).toBe(false);
});

it("keeps progress without an item id on the playback prefix instead of the media surface", () => {
  client.setQueryData(["home", "continue-watching"], { value: "old" });
  client.setQueryData(["playback", "item-1"], { value: "old" });
  for (const key of progressUntouchedKeys) client.setQueryData(key, { value: "old" });
  renderShell();
  markReady();
  act(() => socket().message({ type: "invalidate", channel: "user", scope: "progress" }));
  expect(client.getQueryState(["home", "continue-watching"])?.isInvalidated).toBe(true);
  expect(client.getQueryState(["playback", "item-1"])?.isInvalidated).toBe(true);
  expect(client.getQueryState(["playback", "item-2"])?.isInvalidated).toBe(true);
  for (const key of progressUntouchedKeys.filter((key) => key[0] !== "playback")) {
    expect(client.getQueryState(key)?.isInvalidated).toBe(false);
  }
});

it("stops reconnecting after 1008 and expires the session query", async () => {
  const invalidate = vi.spyOn(client, "invalidateQueries");
  renderShell();
  markReady();
  act(() => socket().serverClose(1008));
  await advance(5 * 60_000);
  expect(FakeWebSocket.instances).toHaveLength(1);
  expect(invalidate).toHaveBeenCalledWith({ queryKey: queryKeys.me });
});

it("checks the session once after five consecutive abnormal closes", async () => {
  const me = vi.spyOn(api, "me").mockRejectedValue(new ApiError("未登录", { status: 401 }));
  const invalidate = vi.spyOn(client, "invalidateQueries");
  renderShell();

  for (let attempt = 0; attempt < 4; attempt += 1) {
    markReady();
    act(() => socket().serverClose(1006));
    await advance(500);
  }
  expect(me).not.toHaveBeenCalled();

  markReady();
  act(() => socket().serverClose(1006));
  await advance(500);
  expect(me).toHaveBeenCalledTimes(1);
  // A 401 means the cookie is gone: the session is treated like a 1008 and `me` is re-read.
  expect(invalidate).toHaveBeenCalledWith({ queryKey: queryKeys.me });
  await advance(5 * 60_000);
  expect(FakeWebSocket.instances).toHaveLength(5);
});
