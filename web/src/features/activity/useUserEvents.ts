import { useQueryClient, type QueryKey } from "@tanstack/react-query";
import { createContext, useCallback, useContext, useEffect, useRef, useState } from "react";
import { clearHomeCarouselCache } from "../home/carousel";
import { queryKeys } from "../../lib/api/query-keys";
import { realtimeConnectionState, subscribeRealtime } from "../../lib/realtime/client";
import type { InvalidateFrame, RealtimeConnectionState } from "../../lib/realtime/types";

export type { RealtimeConnectionState } from "../../lib/realtime/types";

type UserEvent = {
  scope: "home" | "item" | "library" | "favorites" | "progress";
  itemId?: string;
};

export const RealtimeConnectionContext = createContext<RealtimeConnectionState>("disconnected");

export function useRealtimeConnectionState(): RealtimeConnectionState {
  return useContext(RealtimeConnectionContext);
}

export function useUserEvents() {
  const queryClient = useQueryClient();
  const [connectionState, setConnectionState] = useState<RealtimeConnectionState>(realtimeConnectionState);
  const pendingRefreshes = useRef(new Set<string>());
  const hadConnectionError = useRef(false);
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
    const invalidate = (event: UserEvent) => {
      // Progress ticks only move the user's resume pointer and the playing
      // item's playback state; refreshing the whole media surface for every
      // 10s report is what the server-side per-user filtering exists to avoid.
      if (event.scope === "progress") {
        refreshQueries(queryKeys.homeContinueWatching);
        refreshQueries(event.itemId ? queryKeys.playback(event.itemId) : ["playback"]);
        return;
      }
      clearHomeCarouselCache();
      // Legacy home notifications also cover metadata, images and user state.
      // The server may send scope-only events; IDs narrow updates when available.
      const itemId = event.scope === "item" ? event.itemId : undefined;
      // ADR-054 D14: the server never sends `libraryId`, so a library event always widens to
      // the whole `["library"]` prefix. The old per-library narrowing branch was unreachable
      // and is deleted rather than migrated.
      void queryClient.invalidateQueries({
        queryKey: ["library"],
        refetchType: "active",
      });
      const keys: QueryKey[] = [
        queryKeys.home, queryKeys.libraries,
        itemId ? queryKeys.item(itemId) : ["item"],
        itemId ? queryKeys.itemImages(itemId) : ["item-images"],
        itemId ? queryKeys.playback(itemId) : ["playback"],
        ["children"], queryKeys.favorites, ["collection"], ["person"],
      ];
      for (const key of keys) refreshQueries(key);
    };

    const unsubscribe = subscribeRealtime({
      // ADR-054 D15①: a first connection has nothing to catch up on. Only a connection that
      // had dropped (and therefore may have missed invalidations) forces one full home refresh.
      onReady: () => {
        const reconnecting = hadConnectionError.current;
        hadConnectionError.current = false;
        if (reconnecting) invalidate({ scope: "home" });
      },
      onInvalidate: (frame) => {
        const payload = parseUserEvent(frame);
        if (payload) invalidate(payload);
      },
      onStateChange: (state) => {
        if (state === "disconnected") hadConnectionError.current = true;
        setConnectionState(state);
      },
      // A revoked session must land on the login page; `App` derives that from queryKeys.me.
      onSessionEnded: () => {
        void queryClient.invalidateQueries({ queryKey: queryKeys.me });
      },
    });
    // A sibling subscriber may already own a connected socket when this hook mounts.
    setConnectionState(realtimeConnectionState());
    return unsubscribe;
  }, [refreshQueries, queryClient]);

  return connectionState;
}

function parseUserEvent(frame: InvalidateFrame): UserEvent | null {
  if (frame.channel !== "user") return null;
  const scope = frame.scope;
  if (!["home", "item", "library", "favorites", "progress"].includes(scope)) return null;
  if (frame.itemId !== undefined
    && (typeof frame.itemId !== "string" || !frame.itemId.trim() || frame.itemId.length > 256)) {
    return null;
  }
  return { scope: scope as UserEvent["scope"], itemId: frame.itemId };
}
