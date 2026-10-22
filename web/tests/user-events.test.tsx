// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { LuxShell } from "../src/components/layout/LuxShell";
import { useSseConnectionState } from "../src/features/activity/useUserEvents";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

class FakeEventSource extends EventTarget {
  static instances: FakeEventSource[] = [];
  closed = false;
  constructor(readonly url: string) { super(); FakeEventSource.instances.push(this); }
  close() { this.closed = true; }
  emit(type: string, data?: unknown) {
    this.dispatchEvent(new MessageEvent(type, { data: JSON.stringify(data) }));
  }
}

function ConnectionProbe() {
  return <output>{useSseConnectionState()}</output>;
}

let container: HTMLDivElement;
let root: Root;
let client: QueryClient;

beforeEach(() => {
  FakeEventSource.instances = [];
  vi.stubGlobal("EventSource", FakeEventSource);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
});

afterEach(() => {
  act(() => root.unmount());
  client.clear();
  container.remove();
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

it("shares one SSE connection and returns to disconnected on error", () => {
  renderShell();
  expect(container.querySelector("output")?.textContent).toBe("disconnected");
  const source = FakeEventSource.instances[0];
  act(() => source.emit("open"));
  expect(container.querySelector("output")?.textContent).toBe("connected");
  act(() => source.emit("error"));
  expect(container.querySelector("output")?.textContent).toBe("disconnected");
  act(() => source.emit("open"));
  expect(container.querySelector("output")?.textContent).toBe("connected");
  expect(FakeEventSource.instances).toHaveLength(1);
  act(() => root.unmount());
  expect(source.closed).toBe(true);
});

it("keeps fallback mode when EventSource is unavailable", () => {
  vi.stubGlobal("EventSource", undefined);
  renderShell();
  expect(container.querySelector("output")?.textContent).toBe("disconnected");
  expect(FakeEventSource.instances).toHaveLength(0);
});

const mediaKeys = [["home", "carousel"], ["libraries"], ["library", "library-1"],
  ["item", "item-1"], ["item-images", "item-1"], ["playback", "item-1"],
  ["children", "series-1", "EPISODE"], ["favorites"], ["collection", "collection-1"]];

it("does not invalidate loaded media caches on the initial SSE connection", () => {
  for (const key of mediaKeys) client.setQueryData(key, { value: "fresh" });
  renderShell();
  const source = FakeEventSource.instances[0];
  act(() => source.emit("open"));
  for (const key of mediaKeys) expect(client.getQueryState(key)?.isInvalidated).toBe(false);
  act(() => source.emit("error"));
  act(() => source.emit("open"));
  for (const key of mediaKeys) expect(client.getQueryState(key)?.isInvalidated).toBe(true);
});

it.each(["home", "item", "library", "favorites"])("refreshes media caches for a %s event without touching player sessions", (scope) => {
  for (const key of mediaKeys) client.setQueryData(key, { value: "old" });
  const playerKey = ["web-playback-session", "item-1"];
  client.setQueryData(playerKey, { sessionId: "active-session" });
  renderShell();
  act(() => FakeEventSource.instances[0].emit("invalidate", { scope }));
  for (const key of mediaKeys) expect(client.getQueryState(key)?.isInvalidated).toBe(true);
  expect(client.getQueryState(playerKey)?.isInvalidated).toBe(false);
});

it("scopes a library event to that library and does not refetch pages in the background", async () => {
  client.setQueryData(["library", "library-1"], { value: "old" });
  client.setQueryData(["library", "library-2"], { value: "other" });
  const firstKey = ["library", "library-1"];
  let refetchCount = 0;
  client.setQueryDefaults(firstKey, { queryFn: async () => { refetchCount += 1; return { value: "new" }; } });
  renderShell();
  act(() => FakeEventSource.instances[0].emit("invalidate", { scope: "library", libraryId: "library-1" }));
  await act(async () => { await Promise.resolve(); });
  expect(client.getQueryState(firstKey)?.isInvalidated).toBe(true);
  expect(client.getQueryState(["library", "library-2"])?.isInvalidated).toBe(false);
  expect(refetchCount).toBe(0);
});

it("ignores malformed and unknown invalidations", () => {
  client.setQueryData(["item", "item-1"], { title: "old" });
  renderShell();
  for (const payload of [null, {}, { scope: "unknown" }, { scope: "item", itemId: 12 }]) {
    act(() => FakeEventSource.instances[0].emit("invalidate", payload));
  }
  expect(client.getQueryState(["item", "item-1"])?.isInvalidated).toBe(false);
});

it("limits a targeted item event to that item and related list caches", () => {
  client.setQueryData(["item", "item-1"], { title: "old" });
  client.setQueryData(["item", "item-2"], { title: "other" });
  renderShell();
  act(() => FakeEventSource.instances[0].emit("invalidate", { scope: "item", itemId: "item-1" }));
  expect(client.getQueryState(["item", "item-1"])?.isInvalidated).toBe(true);
  expect(client.getQueryState(["item", "item-2"])?.isInvalidated).toBe(false);
});
