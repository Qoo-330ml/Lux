// @vitest-environment jsdom

import { QueryClient, QueryClientProvider, useQuery } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { LuxShell } from "../src/components/layout/LuxShell";
import { AdminLayout } from "../src/features/admin/AdminLayout";
import { FakeWebSocket, installFakeWebSocket } from "./support/fake-websocket";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

/** Stands in for an admin page that subscribes to two of the `jobs` scope query keys. */
function SubscribedAdminQueries() {
  useQuery({
    queryKey: ["admin", "metadata-jobs", "all", "base"],
    queryFn: () => new Promise<never>(() => {}),
  });
  useQuery({ queryKey: ["admin", "logs"], queryFn: () => new Promise<never>(() => {}) });
  return null;
}

function socket(index = -1): FakeWebSocket {
  const ws = FakeWebSocket.instances.at(index);
  if (!ws) throw new Error("no realtime socket was created");
  return ws;
}

describe("admin realtime events", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    installFakeWebSocket();
    vi.spyOn(Math, "random").mockReturnValue(0.5);
    vi.useFakeTimers();
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

  it("opens one websocket and invalidates the queries the mounted view subscribes to", async () => {
    const queryClient = renderAdminLayout(true);
    const invalidate = vi.spyOn(queryClient, "invalidateQueries").mockResolvedValue();
    await act(async () => await Promise.resolve());

    expect(FakeWebSocket.instances).toHaveLength(1);
    expect(socket().url).toBe("ws://localhost:3000/api/v1/ws");

    act(() => socket().message({ type: "ready", version: 1 }));
    act(() => vi.advanceTimersByTime(250));
    expect(invalidate.mock.calls.map(([options]) => options)).toEqual([
      { queryKey: ["admin", "metadata-jobs", "all", "base"], exact: true },
      { queryKey: ["admin", "logs"], exact: true },
    ]);

    invalidate.mockClear();
    act(() => socket().message({ type: "invalidate", channel: "admin", scope: "jobs" }));
    act(() => vi.advanceTimersByTime(250));
    // The eight other job queries no mounted admin view observes are left alone instead of
    // being refreshed (and, with the metadata list, aggregated) once per second.
    expect(invalidate.mock.calls.map(([options]) => options)).toEqual([
      { queryKey: ["admin", "metadata-jobs", "all", "base"], exact: true },
      { queryKey: ["admin", "logs"], exact: true },
    ]);
  });

  it("refreshes every subscribed query on each open, not only the first one (D15②)", async () => {
    const queryClient = renderAdminLayout(true);
    const invalidate = vi.spyOn(queryClient, "invalidateQueries").mockResolvedValue();
    await act(async () => await Promise.resolve());

    act(() => socket().ready());
    act(() => vi.advanceTimersByTime(250));
    expect(invalidate).toHaveBeenCalledTimes(2);

    invalidate.mockClear();
    act(() => socket().serverClose(1006));
    act(() => vi.advanceTimersByTime(500));
    expect(FakeWebSocket.instances).toHaveLength(2);

    act(() => socket().ready());
    act(() => vi.advanceTimersByTime(250));
    expect(invalidate.mock.calls.map(([options]) => options)).toEqual([
      { queryKey: ["admin", "metadata-jobs", "all", "base"], exact: true },
      { queryKey: ["admin", "logs"], exact: true },
    ]);
  });

  it("merges invalidations that arrive inside the same 250ms window (D15③)", async () => {
    const queryClient = renderAdminLayout(true);
    const invalidate = vi.spyOn(queryClient, "invalidateQueries").mockResolvedValue();
    await act(async () => await Promise.resolve());
    act(() => socket().ready());
    act(() => vi.advanceTimersByTime(250));
    invalidate.mockClear();

    act(() => socket().message({ type: "invalidate", channel: "admin", scope: "jobs" }));
    act(() => vi.advanceTimersByTime(100));
    act(() => socket().message({ type: "invalidate", channel: "admin", scope: "metadata" }));
    act(() => vi.advanceTimersByTime(100));
    expect(invalidate).not.toHaveBeenCalled();

    act(() => vi.advanceTimersByTime(50));
    expect(invalidate.mock.calls.map(([options]) => options)).toEqual([
      { queryKey: ["admin", "metadata-jobs", "all", "base"], exact: true },
      { queryKey: ["admin", "logs"], exact: true },
    ]);

    invalidate.mockClear();
    act(() => socket().message({ type: "invalidate", channel: "admin", scope: "metadata" }));
    act(() => vi.advanceTimersByTime(249));
    expect(invalidate).not.toHaveBeenCalled();
    act(() => vi.advanceTimersByTime(1));
    expect(invalidate.mock.calls.map(([options]) => options)).toEqual([
      { queryKey: ["admin", "logs"], exact: true },
    ]);
  });

  it("ignores user-channel frames and malformed scopes", async () => {
    const queryClient = renderAdminLayout(true);
    const invalidate = vi.spyOn(queryClient, "invalidateQueries").mockResolvedValue();
    await act(async () => await Promise.resolve());
    act(() => socket().ready());
    act(() => vi.advanceTimersByTime(250));
    invalidate.mockClear();

    act(() => socket().message({ type: "invalidate", channel: "user", scope: "home" }));
    act(() => socket().message({ type: "invalidate", channel: "admin", scope: "unknown" }));
    act(() => socket().message("not-json"));
    act(() => vi.advanceTimersByTime(250));
    expect(invalidate).not.toHaveBeenCalled();
  });

  it("does not refetch job queries no admin view is subscribed to", async () => {
    const queryClient = renderAdminLayout(false);
    const invalidate = vi.spyOn(queryClient, "invalidateQueries").mockResolvedValue();
    await act(async () => await Promise.resolve());

    act(() => socket().message({ type: "invalidate", channel: "admin", scope: "jobs" }));
    act(() => vi.advanceTimersByTime(250));
    expect(invalidate).not.toHaveBeenCalled();
  });

  it("closes the socket when the admin layout unmounts", () => {
    renderAdminLayout(false);

    const ws = socket();
    act(() => root.unmount());
    expect(ws.closeCalls).toHaveLength(1);
  });

  it("does not refresh metadata jobs twice for metadata audit events", async () => {
    const queryClient = renderAdminLayout(true);
    const invalidate = vi.spyOn(queryClient, "invalidateQueries").mockResolvedValue();
    await act(async () => await Promise.resolve());

    act(() => socket().message({ type: "invalidate", channel: "admin", scope: "metadata" }));
    act(() => vi.advanceTimersByTime(250));
    expect(invalidate.mock.calls.map(([options]) => options)).toEqual([
      { queryKey: ["admin", "logs"], exact: true },
    ]);
  });

  it("shares one socket between the user shell and the admin layout", async () => {
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const invalidate = vi.spyOn(queryClient, "invalidateQueries").mockResolvedValue();
    act(() => {
      root = createRoot(container);
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/admin"]}>
            <Routes>
              <Route element={<LuxShell user={{ id: "operator", usernameNormalized: "operator" }} />}>
                <Route path="/admin" element={<AdminLayout />}>
                  <Route index element={<SubscribedAdminQueries />} />
                </Route>
              </Route>
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => await Promise.resolve());

    // ADR-054: user and admin channels ride the same connection, so an admin tab holds one.
    expect(FakeWebSocket.instances).toHaveLength(1);
    act(() => socket().ready());
    act(() => vi.advanceTimersByTime(250));
    expect(invalidate).toHaveBeenCalled();

    act(() => socket().message({ type: "invalidate", channel: "admin", scope: "jobs" }));
    act(() => vi.advanceTimersByTime(250));
    expect(FakeWebSocket.instances).toHaveLength(1);
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
