import { useQueryClient } from "@tanstack/react-query";
import { createContext, useCallback, useContext, useEffect, useRef, useState } from "react";
import { clearHomeCarouselCache } from "../home/carousel";
import { queryKeys } from "../../lib/api/query-keys";

type UserEventScope = "home";

export type SseConnectionState = "connected" | "disconnected";
export const SseConnectionContext = createContext<SseConnectionState>("disconnected");

export function useSseConnectionState(): SseConnectionState {
  return useContext(SseConnectionContext);
}

export function useUserEvents() {
  const queryClient = useQueryClient();
  const [connectionState, setConnectionState] = useState<SseConnectionState>("disconnected");
  const pendingHomeRefreshes = useRef(new Set<string>());
  const refreshHome = useCallback(() => {
    const homeQueries = queryClient.getQueryCache().findAll({ queryKey: queryKeys.home });
    if (!homeQueries.length) {
      void queryClient.invalidateQueries({
        queryKey: queryKeys.home,
        refetchType: "none",
      });
      return;
    }
    for (const query of homeQueries) {
      const filters = { queryKey: query.queryKey, exact: true };
      if (query.state.error && query.state.data === undefined) {
        void queryClient.invalidateQueries({ ...filters, refetchType: "none" });
      } else if (query.state.fetchStatus === "fetching") {
        if (pendingHomeRefreshes.current.has(query.queryHash)) continue;
        pendingHomeRefreshes.current.add(query.queryHash);
        void queryClient
          .invalidateQueries(filters, { cancelRefetch: false })
          .finally(() => {
            pendingHomeRefreshes.current.delete(query.queryHash);
            const current = queryClient.getQueryCache().find({
              queryKey: query.queryKey,
              exact: true,
            });
            if (current?.state.data !== undefined && !current.state.error) {
              void queryClient.invalidateQueries(filters);
            }
          });
      } else {
        void queryClient.invalidateQueries(filters, { cancelRefetch: false });
      }
    }
  }, [queryClient]);

  useEffect(() => {
    if (typeof EventSource === "undefined") return undefined;

    const source = new EventSource("/api/v1/events");
    const invalidate = (scope: UserEventScope) => {
      if (scope !== "home") return;
      clearHomeCarouselCache();
      refreshHome();
      void queryClient.invalidateQueries({ queryKey: queryKeys.libraries });
      void queryClient.invalidateQueries({ queryKey: ["library"] });
    };
    const handleOpen = () => {
      setConnectionState("connected");
      invalidate("home");
    };
    const handleError = () => setConnectionState("disconnected");
    const handleInvalidate = (event: Event) => {
      const scope = parseScope((event as MessageEvent<string>).data);
      if (scope) invalidate(scope);
    };

    source.addEventListener("open", handleOpen);
    source.addEventListener("error", handleError);
    source.addEventListener("invalidate", handleInvalidate);
    return () => {
      source.removeEventListener("open", handleOpen);
      source.removeEventListener("error", handleError);
      source.removeEventListener("invalidate", handleInvalidate);
      source.close();
    };
  }, [queryClient, refreshHome]);

  return connectionState;
}

function parseScope(data: string): UserEventScope | null {
  try {
    const payload: unknown = JSON.parse(data);
    if (!payload || typeof payload !== "object" || !("scope" in payload)) return null;
    return payload.scope === "home" ? "home" : null;
  } catch {
    return null;
  }
}
