// @vitest-environment jsdom

/**
 * LUX-898: the admin console turns its fallback polling off while the admin channel is connected.
 *
 * The admin channel rides the page's single `/api/v1/ws` connection (ADR-054 D1), so a test has to
 * own that connection the way `LuxShell` does and then decide when it is ready and when it drops.
 * `SharedConnection` below is that owner; the admin pages only observe it.
 */

import { QueryClient, QueryClientProvider, useQueryClient, type QueryKey } from "@tanstack/react-query";
import { act, useEffect, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { LuxShell } from "../src/components/layout/LuxShell";
import { ScanActivityPopover } from "../src/features/activity/ScanActivityPopover";
import {
  RealtimeConnectionContext,
  type RealtimeConnectionState,
} from "../src/features/activity/useUserEvents";
import { AdminDashboardPage } from "../src/features/admin/AdminDashboardPage";
import { AdminLibraryDeletionPanel } from "../src/features/admin/AdminLibraryDeletionPanel";
import { AdminOperationsPage } from "../src/features/admin/AdminOperationsPage";
import { AdminTranscodingPage } from "../src/features/admin/AdminTranscodingPage";
import { AdminUserContentIndexStatus } from "../src/features/admin/user-management/detail/ContentIndexStatus";
import { api } from "../src/lib/api/client";
import { queryKeys, queryRefreshIntervals } from "../src/lib/api/query-keys";
import { subscribeRealtime } from "../src/lib/realtime/client";
import { FakeWebSocket, installFakeWebSocket } from "./support/fake-websocket";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

/** Owns the page's single socket, like `LuxShell` does through the user channel. */
function SharedConnection() {
  const queryClient = useQueryClient();
  useEffect(() => subscribeRealtime(queryClient, { onReady() {}, onInvalidate() {} }), [queryClient]);
  return null;
}

function socket(): FakeWebSocket {
  const ws = FakeWebSocket.last();
  if (!ws) throw new Error("no realtime socket was created");
  return ws;
}

describe("admin fallback polling", () => {
  let container: HTMLDivElement;
  let root: Root;
  let queryClient: QueryClient;

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

  function renderPage(
    element: ReactNode,
    options: { connection?: boolean; userConnectionState?: RealtimeConnectionState } = {},
  ) {
    queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => {
      root = createRoot(container);
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            {options.connection ? <SharedConnection /> : null}
            <RealtimeConnectionContext.Provider value={options.userConnectionState ?? "disconnected"}>
              {element}
            </RealtimeConnectionContext.Provider>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
  }

  const intervalOf = (queryKey: QueryKey) =>
    queryClient.getQueryCache().find({ queryKey })?.options.refetchInterval;

  it("slows the dashboard down instead of stopping it when the admin channel connects", () => {
    // The dashboard carries per-request samples (uptime, CPU, memory, storage) that no admin frame
    // refreshes, so a connected channel only stretches the interval; it never turns it off.
    vi.spyOn(api, "adminDashboard").mockReturnValue(new Promise(() => {}));
    renderPage(<AdminDashboardPage />, { connection: true });

    expect(intervalOf(queryKeys.adminDashboard)).toBe(queryRefreshIntervals.liveDashboard);

    act(() => socket().ready());
    expect(intervalOf(queryKeys.adminDashboard)).toBe(queryRefreshIntervals.liveDashboardConnected);
    expect(queryRefreshIntervals.liveDashboardConnected).toBeGreaterThan(queryRefreshIntervals.liveDashboard);
  });

  it("does not trust the user channel's connection state for admin polling", () => {
    vi.spyOn(api, "adminDashboard").mockReturnValue(new Promise(() => {}));
    // The user channel context says "connected", but no admin channel exists on this page.
    renderPage(<AdminDashboardPage />, { userConnectionState: "connected" });

    expect(FakeWebSocket.instances).toHaveLength(0);
    expect(intervalOf(queryKeys.adminDashboard)).toBe(queryRefreshIntervals.liveDashboard);
  });

  it("restores the dashboard interval after the connection drops", () => {
    vi.spyOn(api, "adminDashboard").mockReturnValue(new Promise(() => {}));
    renderPage(<AdminDashboardPage />, { connection: true });
    act(() => socket().ready());
    expect(intervalOf(queryKeys.adminDashboard)).toBe(queryRefreshIntervals.liveDashboardConnected);

    act(() => socket().serverClose(1006));
    expect(intervalOf(queryKeys.adminDashboard)).toBe(queryRefreshIntervals.liveDashboard);
  });

  it("returns the dashboard to the slow interval when the connection comes back", () => {
    vi.spyOn(api, "adminDashboard").mockReturnValue(new Promise(() => {}));
    renderPage(<AdminDashboardPage />, { connection: true });
    act(() => socket().ready());
    act(() => socket().serverClose(1006));
    expect(intervalOf(queryKeys.adminDashboard)).toBe(queryRefreshIntervals.liveDashboard);

    // The client reconnects after 500 ms + jitter; the first `ready` of the new connection is what
    // counts as connected again.
    act(() => vi.advanceTimersByTime(500));
    act(() => socket().ready());
    expect(intervalOf(queryKeys.adminDashboard)).toBe(queryRefreshIntervals.liveDashboardConnected);
  });

  it("restores the dashboard interval after a 1008 close without reconnecting", () => {
    vi.spyOn(api, "adminDashboard").mockReturnValue(new Promise(() => {}));
    renderPage(<AdminDashboardPage />, { connection: true });
    act(() => socket().ready());
    expect(intervalOf(queryKeys.adminDashboard)).toBe(queryRefreshIntervals.liveDashboardConnected);

    // 1008 means the session is gone: the admin channel is down for good, so the fallback returns
    // and the client must not keep reconnecting.
    act(() => socket().serverClose(1008));
    expect(intervalOf(queryKeys.adminDashboard)).toBe(queryRefreshIntervals.liveDashboard);
    act(() => vi.advanceTimersByTime(60_000));
    expect(FakeWebSocket.instances).toHaveLength(1);
  });

  it("keeps the task activity popover's interval independent of the admin channel", async () => {
    vi.spyOn(api, "adminTaskActivity").mockResolvedValue({
      activities: [{
        id: "scan-job-1",
        kind: "scan",
        taskType: "RECONCILE_LIBRARY",
        libraryId: "library-1",
        status: "RUNNING",
        processedCount: 2,
        totalCount: 10,
      }],
    });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [{ id: "library-1", name: "电影库" }] });
    renderPage(<ScanActivityPopover />, { connection: true });
    await act(async () => {
      await vi.waitFor(() =>
        expect(container.querySelector("button[aria-label*='后台任务活动']")).not.toBeNull());
    });

    act(() => container.querySelector<HTMLButtonElement>("button[aria-label*='后台任务活动']")?.click());
    expect(intervalOf(queryKeys.adminTaskActivity)).toBe(5_000);

    // LUX-898 review: the popover stays on its baseline 5 s fallback. It is mounted by the user
    // shell and several of the jobs it shows (STRM probe, chapter, danmaku, cover) never publish a
    // `jobs` frame, so trusting a "connected" admin channel would freeze those rows.
    act(() => socket().ready());
    expect(intervalOf(queryKeys.adminTaskActivity)).toBe(5_000);

    act(() => socket().serverClose(1006));
    expect(intervalOf(queryKeys.adminTaskActivity)).toBe(5_000);
  });

  it("slows the four runtime-record job lists to 30 s while connected", () => {
    vi.spyOn(api, "adminScheduledTasks").mockReturnValue(new Promise(() => {}));
    vi.spyOn(api, "adminScheduledTaskPlans").mockReturnValue(new Promise(() => {}));
    vi.spyOn(api, "adminLibraries").mockReturnValue(new Promise(() => {}));
    vi.spyOn(api, "adminJobs").mockReturnValue(new Promise(() => {}));
    vi.spyOn(api, "adminMetadataReidentifyJobs").mockReturnValue(new Promise(() => {}));
    vi.spyOn(api, "adminStrmProbeJobs").mockReturnValue(new Promise(() => {}));
    vi.spyOn(api, "adminChapterDetectionJobs").mockReturnValue(new Promise(() => {}));
    vi.spyOn(api, "adminDanmakuMatchJobs").mockReturnValue(new Promise(() => {}));
    vi.spyOn(api, "adminLibraryCoverJobs").mockReturnValue(new Promise(() => {}));
    vi.spyOn(api, "adminLogs").mockReturnValue(new Promise(() => {}));

    renderPage(<AdminOperationsPage />, { connection: true });
    const jobKeys = [
      queryKeys.adminStrmProbeJobs(""),
      queryKeys.adminChapterDetectionJobs(""),
      queryKeys.adminDanmakuMatchJobs(""),
      queryKeys.adminLibraryCoverJobs(""),
    ];
    for (const key of jobKeys) expect(intervalOf(key)).toBe(queryRefreshIntervals.adminJobs);

    // Only the scanner and the metadata reidentify service publish `jobs` frames; the four lists
    // below also show work that only writes progress to the database, so a connected channel only
    // stretches the fallback instead of stopping it.
    act(() => socket().ready());
    for (const key of jobKeys) expect(intervalOf(key)).toBe(queryRefreshIntervals.adminJobsConnected);
    expect(queryRefreshIntervals.adminJobsConnected).toBeGreaterThan(queryRefreshIntervals.adminJobs);

    act(() => socket().serverClose(1006));
    for (const key of jobKeys) expect(intervalOf(key)).toBe(queryRefreshIntervals.adminJobs);
  });

  it("does not let a jobs frame refetch the task activity query on a user page", async () => {
    // The popover is mounted by the user shell for administrators, but the shell subscribes only to
    // the user channel: no admin frame may reach `adminTaskActivity` outside the console.
    const activity = vi.spyOn(api, "adminTaskActivity").mockResolvedValue({
      activities: [{
        id: "scan-job-1",
        kind: "scan",
        taskType: "RECONCILE_LIBRARY",
        libraryId: "library-1",
        status: "RUNNING",
        processedCount: 1,
        totalCount: 2,
      }],
    });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [{ id: "library-1", name: "电影库" }] });
    queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => {
      root = createRoot(container);
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/"]}>
            <Routes>
              <Route element={<LuxShell user={{ id: "operator", usernameNormalized: "operator", canManageServer: true }} />}>
                <Route index element={<div>home</div>} />
              </Route>
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await vi.waitFor(() => expect(activity).toHaveBeenCalled());
    });
    expect(FakeWebSocket.instances).toHaveLength(1);

    const calls = activity.mock.calls.length;
    act(() => socket().ready());
    act(() => socket().message({ type: "invalidate", channel: "admin", scope: "jobs" }));
    act(() => vi.advanceTimersByTime(250));
    await act(async () => await Promise.resolve());
    expect(activity).toHaveBeenCalledTimes(calls);
  });

  it("leaves the transcoding session polling at two seconds when connected", () => {
    vi.spyOn(api, "adminSettings").mockReturnValue(new Promise(() => {}));
    vi.spyOn(api, "adminTranscodingSessions").mockReturnValue(new Promise(() => {}));
    vi.spyOn(api, "adminTranscodingHistory").mockReturnValue(new Promise(() => {}));
    renderPage(<AdminTranscodingPage />, { connection: true });
    act(() => socket().ready());

    expect(intervalOf(queryKeys.adminTranscodingSessions)).toBe(2_000);
  });

  it("leaves the library deletion jobs polling unchanged when connected", async () => {
    vi.spyOn(api, "adminLibraryDeletionJobs").mockResolvedValue({
      jobs: [{
        id: "deletion-1",
        libraryId: "library-1",
        libraryName: "旧电影库",
        status: "RUNNING",
        stage: "MEDIA_ITEMS",
        processedCount: 12,
        createdAt: 1_700_000_000,
        updatedAt: 1_700_000_100,
      }],
      page: 1,
      pageSize: 20,
      total: 1,
    });
    renderPage(<AdminLibraryDeletionPanel />, { connection: true });
    act(() => socket().ready());
    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("正在删除的媒体库"));
    });

    const interval = intervalOf(queryKeys.adminLibraryDeletionJobs("OPEN", 1)) as unknown as (
      query: { state: { data: { jobs: unknown[] } } },
    ) => number | false;
    expect(interval({ state: { data: { jobs: [{}] } } })).toBe(5_000);
    expect(interval({ state: { data: { jobs: [] } } })).toBe(15_000);
  });

  it("leaves the content index polling unchanged when connected", async () => {
    vi.spyOn(api, "adminContentRestrictionIndexStatus").mockResolvedValue({
      totalItems: 10,
      indexedItems: 4,
      pendingItems: 6,
    });
    renderPage(<AdminUserContentIndexStatus />, { connection: true });
    act(() => socket().ready());
    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("正在回填"));
    });

    const interval = intervalOf(["admin", "content-restriction-index"]);
    expect(typeof interval).toBe("function");
    // The option is a function because the interval depends on `pendingItems`; pin the value it
    // returns while the backfill is incomplete.
    const pendingInterval = interval as unknown as (
      query: { state: { data: { pendingItems: number } } },
    ) => number | false;
    expect(pendingInterval({ state: { data: { pendingItems: 6 } } })).toBe(5_000);
  });
});
