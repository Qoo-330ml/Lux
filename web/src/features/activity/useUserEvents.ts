import { useQueryClient, type QueryKey } from "@tanstack/react-query";
import { createContext, useCallback, useContext, useEffect, useRef, useState } from "react";
import { clearHomeCarouselCache } from "../home/carousel";
import { queryKeys } from "../../lib/api/query-keys";

type UserEvent = { scope: "home" | "item" | "library" | "favorites"; itemId?: string; libraryId?: string };

export type SseConnectionState = "connected" | "disconnected";
export const SseConnectionContext = createContext<SseConnectionState>("disconnected");

export function useSseConnectionState(): SseConnectionState {
  return useContext(SseConnectionContext);
}

export function useUserEvents() {
  const queryClient = useQueryClient();
  const [connectionState, setConnectionState] = useState<SseConnectionState>("disconnected");
  const pendingRefreshes = useRef(new Set<string>());
  const refreshQueries = useCallback((queryKey: QueryKey) => {
    const queries = queryClient.getQueryCache().findAll({ queryKey });
    if (!queries.length) {
      void queryClient.invalidateQueries({
        queryKey,
        refetchType: "none",
      });
      return;
    }
    for (const query of queries) {
      const filters = { queryKey: query.queryKey, exact: true };
      if (queryKey[0] === "home" && query.state.error && query.state.data === undefined) {
        void queryClient.invalidateQueries({ ...filters, refetchType: "none" });
      } else if (query.state.fetchStatus === "fetching") {
        if (pendingRefreshes.current.has(query.queryHash)) continue;
        pendingRefreshes.current.add(query.queryHash);
        void queryClient
          .invalidateQueries(filters, { cancelRefetch: false })
          .finally(() => {
            pendingRefreshes.current.delete(query.queryHash);
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
    const invalidate = (event: UserEvent) => {
      clearHomeCarouselCache();
      // Legacy home notifications also cover metadata, images and user state.
      // The server may send scope-only events; IDs narrow updates when available.
      const itemId = event.scope === "item" ? event.itemId : undefined;
      const libraryId = event.scope === "library" ? event.libraryId : undefined;
      const keys: QueryKey[] = [
        queryKeys.home, queryKeys.libraries,
        libraryId ? ["library", libraryId] : ["library"],
        itemId ? queryKeys.item(itemId) : ["item"],
        itemId ? queryKeys.itemImages(itemId) : ["item-images"],
        itemId ? queryKeys.playback(itemId) : ["playback"],
        ["children"], queryKeys.favorites, ["collection"], ["person"],
      ];
      for (const key of keys) refreshQueries(key);
    };
    const handleOpen = () => {
      setConnectionState("connected");
      invalidate({ scope: "home" });
    };
    const handleError = () => setConnectionState("disconnected");
    const handleInvalidate = (event: Event) => {
      const payload = parseUserEvent((event as MessageEvent<string>).data);
      if (payload) invalidate(payload);
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
  }, [refreshQueries]);

  return connectionState;
}

function parseUserEvent(data: string): UserEvent | null {
  try {
    const payload: unknown = JSON.parse(data);
    if (!payload || typeof payload !== "object" || !("scope" in payload)) return null;
    if (typeof payload.scope !== "string" || !["home", "item", "library", "favorites"].includes(payload.scope)) return null;
    for (const key of ["itemId", "libraryId"] as const) {
      if (key in payload) {
        const value = (payload as Record<string, unknown>)[key];
        if (typeof value !== "string" || !value.trim() || value.length > 256) return null;
      }
    }
    return payload as UserEvent;
  } catch {
    return null;
  }
}
