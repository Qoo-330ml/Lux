import { useQueryClient, type QueryKey } from "@tanstack/react-query";
import { useEffect } from "react";

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
    ["admin", "metadata-jobs"],
    ["admin", "logs"],
  ],
  settings: [
    ["admin", "settings"],
    ["admin", "dashboard"],
    ["admin", "logs"],
  ],
};

export function useAdminEvents() {
  const queryClient = useQueryClient();

  useEffect(() => {
    const source = new EventSource("/api/v1/admin/events");
    const invalidate = (scope: AdminEventScope) => {
      const keys = scope === "all" ? [adminQueryRoot] : queryKeysByScope[scope];
      for (const queryKey of keys) {
        void queryClient.invalidateQueries({ queryKey });
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
