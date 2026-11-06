import { useQueryClient, type QueryClient, type QueryKey } from "@tanstack/react-query";
import { useEffect, useRef } from "react";
import { realtimeConnectionState, subscribeRealtime } from "../../lib/realtime/client";

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
 * Invalidates only the queries a mounted admin view is currently subscribed to.
 *
 * The scope lists above describe which data a kind of event can make stale, not which pages are
 * open. Refreshing every listed key blindly makes the server answer requests for pages that are
 * not on screen -- the `jobs` stream alone would touch ten keys up to once per second while a job
 * runs. An unmounted page fetches again when it mounts, so it does not need the refresh here.
 */
function invalidateSubscribedQueries(queryClient: QueryClient, queryKeys: QueryKey[]) {
  const subscribed = new Map<string, QueryKey>();
  for (const queryKey of queryKeys) {
    for (const query of queryClient.getQueryCache().findAll({ queryKey, type: "active" })) {
      subscribed.set(JSON.stringify(query.queryKey), query.queryKey);
    }
  }
  for (const queryKey of subscribed.values()) {
    void queryClient.invalidateQueries({ queryKey, exact: true });
  }
}

export function useAdminEvents() {
  const queryClient = useQueryClient();
  const pendingKeys = useRef(new Map<string, QueryKey>());
  const flushTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    const flush = () => {
      flushTimer.current = null;
      const keys = [...pendingKeys.current.values()];
      pendingKeys.current.clear();
      invalidateSubscribedQueries(queryClient, keys);
    };
    const invalidate = (scope: AdminEventScope) => {
      const keys = scope === "all" ? [adminQueryRoot] : queryKeysByScope[scope];
      for (const queryKey of keys) {
        pendingKeys.current.set(JSON.stringify(queryKey), queryKey);
      }
      if (flushTimer.current === null) {
        flushTimer.current = setTimeout(flush, 250);
      }
    };

    const unsubscribe = subscribeRealtime(queryClient, {
      // ADR-054 D15②: the admin channel refreshes everything on every open, including the
      // first one, because the console has no cached "ready" state to trust after a gap.
      onReady: () => invalidate("all"),
      onInvalidate: (frame) => {
        if (frame.channel !== "admin") return;
        const scope = parseScope(frame.scope);
        if (scope) invalidate(scope);
      },
    });
    // ADR-054 D15②: `onReady` fires once per connection, but the console mounts long after the
    // connection is up whenever the user walks from the home page into /admin, and admin frames
    // that arrived while it was unmounted were dropped. Sweeping on mount covers both. This is
    // deliberately local to the admin hook rather than a `ready` replay in the hub: a replay
    // would also reach the user channel, whose "only after a drop" rule (D15①) must stay
    // unambiguous.
    if (realtimeConnectionState() === "connected") invalidate("all");
    return () => {
      unsubscribe();
      if (flushTimer.current !== null) clearTimeout(flushTimer.current);
      flushTimer.current = null;
      pendingKeys.current.clear();
    };
  }, [queryClient]);
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
