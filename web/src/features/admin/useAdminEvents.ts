import { useQueryClient, type QueryClient, type QueryKey } from "@tanstack/react-query";
import { useEffect, useSyncExternalStore } from "react";
import {
  observeRealtime,
  realtimeConnectionState,
  subscribeRealtime,
} from "../../lib/realtime/client";
import type { RealtimeConnectionState } from "../../lib/realtime/types";

type AdminEventScope =
  | "all"
  | "dashboard"
  | "jobs"
  | "libraries"
  | "plugins"
  | "users"
  | "metadata"
  | "settings";

const adminQueryRoot = ["admin"] as const;

const queryKeysByScope: Record<Exclude<AdminEventScope, "all">, QueryKey[]> = {
  dashboard: [
    ["admin", "dashboard"],
    ["admin", "logs"],
  ],
  jobs: [
    ["admin", "jobs"],
    ["admin", "metadata-jobs"],
    ["admin", "strm-probe-jobs"],
    ["admin", "chapter-detection-jobs"],
    ["admin", "danmaku-match-jobs"],
    ["admin", "library-cover-jobs"],
    ["admin", "task-activity"],
    ["admin", "scheduled-tasks"],
    ["admin", "dashboard"],
    ["admin", "logs"],
  ],
  libraries: [
    ["admin", "libraries"],
    ["admin", "scheduled-tasks"],
    ["admin", "dashboard"],
    ["admin", "logs"],
  ],
  plugins: [
    ["admin", "plugins"],
    ["admin", "libraries"],
    ["admin", "logs"],
  ],
  users: [
    ["admin", "users"],
    ["admin", "user-access"],
    ["admin", "libraries"],
    ["admin", "dashboard"],
    ["admin", "logs"],
  ],
  metadata: [
    ["admin", "logs"],
  ],
  settings: [
    ["admin", "settings"],
    ["admin", "dashboard"],
    ["admin", "logs"],
  ],
};

/**
 * Marks the queries an admin event makes stale, refreshing only the ones on screen.
 *
 * The scope lists above describe which data a kind of event can make stale, not which pages are
 * open. Refreshing every listed key blindly makes the server answer requests for pages that are
 * not on screen -- the `jobs` stream alone would touch ten keys up to once per second while a job
 * runs. The queries that are not on screen are still marked stale with `refetchType: "none"`:
 * `web/src/main.tsx` keeps every query fresh for 60 s, so a page that unmounts while a job runs
 * would otherwise keep showing the job as running when it comes back inside that window. An
 * invalidation overrides `staleTime`, so the next mount refetches; nothing is fetched now.
 */
function invalidateSubscribedQueries(queryClient: QueryClient, queryKeys: QueryKey[]) {
  const active = new Map<string, QueryKey>();
  const inactive = new Map<string, QueryKey>();
  for (const queryKey of queryKeys) {
    for (const query of queryClient.getQueryCache().findAll({ queryKey })) {
      const hash = JSON.stringify(query.queryKey);
      const target = query.isActive() ? active : inactive;
      target.set(hash, query.queryKey);
    }
  }
  for (const queryKey of active.values()) {
    void queryClient.invalidateQueries({ queryKey, exact: true });
  }
  for (const queryKey of inactive.values()) {
    void queryClient.invalidateQueries({ queryKey, exact: true, refetchType: "none" });
  }
}

/**
 * The polling fallback an admin query keeps while the admin channel is down (LUX-898).
 *
 * ADR-054 D2 keeps the 2 s / 5 s / 15 s fallbacks for a connection that cannot be established, so
 * they stay exactly as they were while the channel is disconnected. Once it is connected the
 * server's `invalidate` frames are what refreshes the scope-covered queries; a timer would only
 * repeat the request a frame already triggered.
 */
export function adminFallbackInterval(
  state: RealtimeConnectionState,
  interval: number,
): number | false {
  return state === "connected" ? false : interval;
}

/**
 * The admin channel's state and invalidation handler, shared by every admin surface.
 *
 * The channel has no socket of its own: ADR-054 D1 puts the admin frames on the page's single
 * connection, which `LuxShell` owns through the user channel. A surface that needs the admin
 * channel therefore *observes* that connection and never starts or keeps one; only the console
 * layout (`useAdminEvents`) additionally guarantees a connection, for the case where no user
 * channel is mounted. That is also why the user channel's connection state is not used here: a
 * page that never subscribes to the admin channel must keep its polling, and an admin view must
 * not report "connected" just because the user channel happens to be up.
 *
 * There is one registration for all admin views, not one per component: the `ready` sweep
 * (ADR-054 D15②) and the 250 ms merge window (D15③) have to run once per frame regardless of how
 * many admin pages or popovers are mounted.
 */
let channelState: RealtimeConnectionState = "disconnected";
let channelQueryClient: QueryClient | null = null;
let observerTeardown: (() => void) | null = null;
let ownerTeardown: (() => void) | null = null;
let consumers = 0;
let owners = 0;
let flushTimer: ReturnType<typeof setTimeout> | null = null;
const pendingKeys = new Map<string, QueryKey>();
const stateListeners = new Set<() => void>();

function publishChannelState(next: RealtimeConnectionState) {
  if (channelState === next) return;
  channelState = next;
  for (const listener of [...stateListeners]) listener();
}

function subscribeChannelState(listener: () => void) {
  stateListeners.add(listener);
  return () => {
    stateListeners.delete(listener);
  };
}

function currentChannelState(): RealtimeConnectionState {
  return channelState;
}

function flushInvalidations() {
  flushTimer = null;
  const queryClient = channelQueryClient;
  if (!queryClient) return;
  const keys = [...pendingKeys.values()];
  pendingKeys.clear();
  invalidateSubscribedQueries(queryClient, keys);
}

function invalidateScope(scope: AdminEventScope) {
  const keys = scope === "all" ? [adminQueryRoot] : queryKeysByScope[scope];
  for (const queryKey of keys) {
    pendingKeys.set(JSON.stringify(queryKey), queryKey);
  }
  if (flushTimer === null) {
    flushTimer = setTimeout(flushInvalidations, 250);
  }
}

function acquireChannel(queryClient: QueryClient, owner: boolean): () => void {
  consumers += 1;
  if (!channelQueryClient) channelQueryClient = queryClient;
  if (!observerTeardown) {
    observerTeardown = observeRealtime({
      // ADR-054 D15②: the admin channel refreshes everything on every open, including the first
      // one, because the console has no cached "ready" state to trust after a gap.
      onReady: () => invalidateScope("all"),
      onInvalidate: (frame) => {
        if (frame.channel !== "admin") return;
        const scope = parseScope(frame.scope);
        if (scope) invalidateScope(scope);
      },
      onStateChange: publishChannelState,
    });
    // The observer is not replayed, so pick up a connection that is already ready.
    publishChannelState(realtimeConnectionState());
  }
  if (owner) {
    owners += 1;
    if (!ownerTeardown) {
      // The console keeps a connection alive even when no user channel is mounted. Frames and
      // state are handled once by the observer above, so this registration only owns the socket's
      // lifecycle: no handler may invalidate a query twice.
      ownerTeardown = subscribeRealtime(queryClient, {
        onReady: () => {},
        onInvalidate: () => {},
      });
    }
  }
  return () => {
    consumers -= 1;
    if (owner) {
      owners -= 1;
      if (owners === 0) {
        ownerTeardown?.();
        ownerTeardown = null;
      }
    }
    if (consumers === 0) teardownChannel();
  };
}

function teardownChannel() {
  observerTeardown?.();
  ownerTeardown?.();
  observerTeardown = null;
  ownerTeardown = null;
  owners = 0;
  if (flushTimer !== null) clearTimeout(flushTimer);
  flushTimer = null;
  pendingKeys.clear();
  channelQueryClient = null;
  publishChannelState("disconnected");
}

function useAdminChannel(queryClient: QueryClient, owner: boolean): RealtimeConnectionState {
  useEffect(() => acquireChannel(queryClient, owner), [queryClient, owner]);
  return useSyncExternalStore(subscribeChannelState, currentChannelState, currentChannelState);
}

/**
 * The admin channel's connection state, for admin views that turn their fallback polling off.
 *
 * `"connected"` means the shared connection delivered `ready` while this admin channel is being
 * observed, so admin `invalidate` frames are being received. It is deliberately not the user
 * channel's state: admin pages only stop polling through this hook.
 *
 * Residual risk (ADR-054 D7): a server-side revalidation that removes the admin role only stops
 * the admin frames and leaves the connection up without any frame to say so, so this hook keeps
 * reporting `"connected"` until the connection drops. The next reconnect, or a polling page the
 * connection never covered, is what surfaces the change.
 */
export function useAdminRealtimeConnectionState(): RealtimeConnectionState {
  const queryClient = useQueryClient();
  return useAdminChannel(queryClient, false);
}

/**
 * Subscribes the admin console to the admin channel and returns its connection state.
 *
 * Mounted by `AdminLayout`, which is the surface that is allowed to open the shared connection on
 * its own (a non-admin never mounts it, so nobody else subscribes to the admin channel).
 */
export function useAdminEvents(): RealtimeConnectionState {
  const queryClient = useQueryClient();
  const state = useAdminChannel(queryClient, true);
  useEffect(() => {
    // ADR-054 D15②: `onReady` fires once per connection, but the console mounts long after the
    // connection is up whenever the user walks from the home page into /admin, and admin frames
    // that arrived while it was unmounted were dropped. Sweeping on mount covers both. This is
    // deliberately local to the admin hook rather than a `ready` replay in the hub: a replay
    // would also reach the user channel, whose "only after a drop" rule (D15①) must stay
    // unambiguous.
    if (realtimeConnectionState() === "connected") invalidateScope("all");
  }, []);
  return state;
}

function parseScope(scope: string): AdminEventScope | null {
  return isAdminEventScope(scope) ? scope : null;
}

function isAdminEventScope(value: unknown): value is AdminEventScope {
  return typeof value === "string" && [
    "all",
    "dashboard",
    "jobs",
    "libraries",
    "plugins",
    "users",
    "metadata",
    "settings",
  ].includes(value);
}
