import type { QueryClient } from "@tanstack/react-query";
import { queryKeys } from "./query-keys";

/**
 * Drops the caches of an ended session and hands the UI back to `App`.
 *
 * The QueryClient is a page-wide singleton (`web/src/main.tsx`) that outlives a login, and most
 * query keys carry no user dimension (`home/*`, `favorites`, `watch-history`, `playback`,
 * `admin/*`). With `staleTime: 60_000` and `refetchOnWindowFocus: false` a second account signing
 * in on the same tab would otherwise keep reading the previous account's continue-watching,
 * history and favorites until those entries go stale. The explicit logout and the realtime
 * "session revoked" path therefore share this one function.
 *
 * `queryKeys.me` is the one query that survives:
 *
 * - it is what `App` uses to decide between the login page and the shell, so it has to be
 *   invalidated rather than dropped — a removed query never notifies its observer, and the shell
 *   would keep rendering the cached user;
 * - its refetch answers `401`, and that is what moves the UI to the login page;
 * - it carries no per-account collection data, and the next login overwrites it.
 */
export function clearSessionCaches(queryClient: QueryClient) {
  queryClient.removeQueries({ predicate: (query) => query.queryKey[0] !== queryKeys.me[0] });
  void queryClient.invalidateQueries({ queryKey: queryKeys.me });
}
