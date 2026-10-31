// @vitest-environment jsdom

import { QueryClient, QueryClientProvider, useQuery } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AdminLayout } from "../src/features/admin/AdminLayout";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

type EventListener = (event: Event) => void;

class FakeEventSource {
  static instances: FakeEventSource[] = [];
  readonly listeners = new Map<string, Set<EventListener>>();
  closed = false;

  constructor(readonly url: string) {
    FakeEventSource.instances.push(this);
  }

  addEventListener(type: string, listener: EventListener) {
    const listeners = this.listeners.get(type) ?? new Set<EventListener>();
    listeners.add(listener);
    this.listeners.set(type, listeners);
  }

  removeEventListener(type: string, listener: EventListener) {
    this.listeners.get(type)?.delete(listener);
  }

  close() {
    this.closed = true;
  }

  emit(type: string, data?: string) {
    const event = new MessageEvent(type, { data });
    for (const listener of this.listeners.get(type) ?? []) listener(event);
  }
}

/** Stands in for an admin page that subscribes to two of the `jobs` scope query keys. */
function SubscribedAdminQueries() {
  useQuery({
    queryKey: ["admin", "metadata-jobs", "all", "base"],
    queryFn: () => new Promise<never>(() => {}),
  });
  useQuery({ queryKey: ["admin", "logs"], queryFn: () => new Promise<never>(() => {}) });
  return null;
}

describe("admin SSE events", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    FakeEventSource.instances = [];
    vi.stubGlobal("EventSource", FakeEventSource);
    container = document.createElement("div");
    document.body.append(container);
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.useRealTimers();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  function renderAdminLayout(subscribed: boolean) {
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => {
      root = createRoot(container);
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/admin"]}>
            <Routes>
              <Route path="/admin" element={<AdminLayout />}>
                <Route index element={subscribed ? <SubscribedAdminQueries /> : null} />
              </Route>
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    return queryClient;
  }

  it("opens one authenticated stream and invalidates the queries the mounted view subscribes to", async () => {
    vi.useFakeTimers();
    const queryClient = renderAdminLayout(true);
    const invalidate = vi.spyOn(queryClient, "invalidateQueries").mockResolvedValue();
    await act(async () => await Promise.resolve());

    expect(FakeEventSource.instances).toHaveLength(1);
    const source = FakeEventSource.instances[0];
    expect(source.url).toBe("/api/v1/admin/events");

    act(() => source.emit("open"));
    act(() => vi.advanceTimersByTime(250));
    expect(invalidate.mock.calls.map(([options]) => options)).toEqual([
      { queryKey: ["admin", "metadata-jobs", "all", "base"], exact: true },
      { queryKey: ["admin", "logs"], exact: true },
    ]);

    invalidate.mockClear();
    act(() => source.emit("invalidate", JSON.stringify({ scope: "jobs" })));
    act(() => vi.advanceTimersByTime(250));
    // The eight other job queries no mounted admin view observes are left alone instead of
    // being refreshed (and, with the metadata list, aggregated) once per second.
    expect(invalidate.mock.calls.map(([options]) => options)).toEqual([
      { queryKey: ["admin", "metadata-jobs", "all", "base"], exact: true },
      { queryKey: ["admin", "logs"], exact: true },
    ]);
  });

  it("does not refetch job queries no admin view is subscribed to", async () => {
    vi.useFakeTimers();
    const queryClient = renderAdminLayout(false);
    const invalidate = vi.spyOn(queryClient, "invalidateQueries").mockResolvedValue();
    await act(async () => await Promise.resolve());

    act(() => FakeEventSource.instances[0].emit("invalidate", JSON.stringify({ scope: "jobs" })));
    act(() => vi.advanceTimersByTime(250));
    expect(invalidate).not.toHaveBeenCalled();
  });

  it("closes the stream when the admin layout unmounts", () => {
    renderAdminLayout(false);

    const source = FakeEventSource.instances[0];
    act(() => root.unmount());
    expect(source.closed).toBe(true);
  });

  it("does not refresh metadata jobs twice for metadata audit events", async () => {
    vi.useFakeTimers();
    const queryClient = renderAdminLayout(true);
    const invalidate = vi.spyOn(queryClient, "invalidateQueries").mockResolvedValue();
    await act(async () => await Promise.resolve());

    act(() => FakeEventSource.instances[0].emit("invalidate", JSON.stringify({ scope: "metadata" })));
    act(() => vi.advanceTimersByTime(250));
    expect(invalidate.mock.calls.map(([options]) => options)).toEqual([
      { queryKey: ["admin", "logs"], exact: true },
    ]);
  });

  it("prefetches a target page when its navigation link is activated", () => {
    const queryClient = renderAdminLayout(false);
    const prefetch = vi.spyOn(queryClient, "prefetchQuery").mockResolvedValue(undefined);

    const pluginsLink = [...container.querySelectorAll<HTMLAnchorElement>("a")]
      .find((link) => link.getAttribute("href") === "/admin/plugins");
    expect(pluginsLink).toBeTruthy();

    act(() => pluginsLink?.dispatchEvent(new Event("pointerdown", { bubbles: true })));

    expect(prefetch).toHaveBeenCalledWith(expect.objectContaining({ queryKey: ["admin", "plugins"] }));
  });
});
