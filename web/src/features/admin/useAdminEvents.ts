import { useQueryClient, type QueryClient, type QueryKey } from "@tanstack/react-query";
import { useEffect, useRef } from "react";

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
    const source = new EventSource("/api/v1/admin/events");
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
    const handleOpen = () => invalidate("all");
    const handleInvalidate = (event: Event) => {
      const data = (event as MessageEvent<string>).data;
      const scope = parseScope(data);
      if (scope) invalidate(scope);
    };

    source.addEventListener("open", handleOpen);
    source.addEventListener("invalidate", handleInvalidate);
    return () => {
      source.removeEventListener("open", handleOpen);
      source.removeEventListener("invalidate", handleInvalidate);
      source.close();
      if (flushTimer.current !== null) clearTimeout(flushTimer.current);
      flushTimer.current = null;
      pendingKeys.current.clear();
    };
  }, [queryClient]);
}

function parseScope(data: string): AdminEventScope | null {
  try {
    const payload: unknown = JSON.parse(data);
    if (!payload || typeof payload !== "object" || !("scope" in payload)) return null;
    const scope = payload.scope;
    return isAdminEventScope(scope) ? scope : null;
  } catch {
    return null;
  }
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
