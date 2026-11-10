import type { QueryClient } from "@tanstack/react-query";
import { queryKeys } from "./query-keys";

/**
 * Query roots that outlive a session.
 *
 * - `me` decides between the login page and the shell in `App`. Dropping it detaches the observer
 *   that reports the `401`, so it is invalidated instead of removed.
 * - `setup` / `setup/database` describe the server before anybody signed in, and `login-background`
 *   is the artwork the login page shows. Removing them makes the app flash "正在启动 Lux" (and land
 *   on the error screen when that request then fails) on its way back to the login page.
 *
 * Everything else in `query-keys.ts` is account-scoped, so this stays a short deny-list: a new
 * user-data key is cleared automatically instead of silently leaking into the next session.
 */
const SESSION_SURVIVING_ROOTS: readonly string[] = [
  queryKeys.me[0],
  queryKeys.setup[0],
  queryKeys.loginBackground[0],
];

/**
 * Drops every account-scoped cached query and leaves the session query alone.
 *
 * The QueryClient is a page-wide singleton (`web/src/main.tsx`) that outlives a login, and most
 * query keys carry no user dimension (`home/*`, `favorites`, `watch-history`, `playback`,
 * `admin/*`). With `staleTime: 60_000` and `refetchOnWindowFocus: false` a second account signing
 * in on the same tab would otherwise keep reading the previous account's continue-watching,
 * history and favorites until those entries go stale.
 *
 * `App` uses this when `/auth/me` answers `401` on its own (a reconnect, or an explicit
 * invalidation): nothing there may touch `me`, because refetching a query that already answers
 * `401` would just repeat it.
 */
export function clearUserCaches(queryClient: QueryClient) {
  queryClient.removeQueries({
    predicate: (query) => !SESSION_SURVIVING_ROOTS.includes(String(query.queryKey[0])),
  });
}

/**
 * The full sign-out: drop the account's caches, then invalidate `me` so its observer re-reads the
 * session, gets `401` and hands the UI to the login page. Shared by the explicit logout and the
 * realtime "session revoked" path.
 */
export function clearSessionCaches(queryClient: QueryClient) {
  clearUserCaches(queryClient);
  void queryClient.invalidateQueries({ queryKey: queryKeys.me });
}
