// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
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
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it("opens one authenticated stream and invalidates matching queries", async () => {
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const invalidate = vi.spyOn(queryClient, "invalidateQueries").mockResolvedValue();

    act(() => {
      root = createRoot(container);
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <AdminLayout />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => await Promise.resolve());

    expect(FakeEventSource.instances).toHaveLength(1);
    const source = FakeEventSource.instances[0];
    expect(source.url).toBe("/api/v1/admin/events");

    act(() => source.emit("open"));
    expect(invalidate).toHaveBeenCalledWith({ queryKey: ["admin"] });

    invalidate.mockClear();
    act(() => source.emit("invalidate", JSON.stringify({ scope: "jobs" })));
    expect(invalidate.mock.calls.map(([options]) => options)).toEqual([
      { queryKey: ["admin", "jobs"] },
      { queryKey: ["admin", "metadata-jobs"] },
      { queryKey: ["admin", "strm-probe-jobs"] },
      { queryKey: ["admin", "chapter-detection-jobs"] },
      { queryKey: ["admin", "danmaku-match-jobs"] },
      { queryKey: ["admin", "library-cover-jobs"] },
      { queryKey: ["admin", "task-activity"] },
      { queryKey: ["admin", "scheduled-tasks"] },
      { queryKey: ["admin", "dashboard"] },
      { queryKey: ["admin", "logs"] },
    ]);
  });

  it("closes the stream when the admin layout unmounts", () => {
    const queryClient = new QueryClient();
    act(() => {
      root = createRoot(container);
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <AdminLayout />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });

    const source = FakeEventSource.instances[0];
    act(() => root.unmount());
    expect(source.closed).toBe(true);
  });

  it("refreshes metadata jobs without a standalone pending-metadata query", async () => {
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const invalidate = vi.spyOn(queryClient, "invalidateQueries").mockResolvedValue();

    act(() => {
      root = createRoot(container);
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <AdminLayout />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => await Promise.resolve());

    act(() => FakeEventSource.instances[0].emit("invalidate", JSON.stringify({ scope: "metadata" })));
    expect(invalidate.mock.calls.map(([options]) => options)).toEqual([
      { queryKey: ["admin", "metadata-jobs"] },
      { queryKey: ["admin", "logs"] },
    ]);
  });
});
