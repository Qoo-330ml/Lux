import { QueryClient } from "@tanstack/react-query";
import { describe, expect, it, vi } from "vitest";
import { queryKeys } from "../src/lib/api/query-keys";
import { clearSessionCaches, clearUserCaches } from "../src/lib/api/session";

function seededClient() {
  const client = new QueryClient();
  client.setQueryData(queryKeys.me, { user: { id: "previous-account" } });
  client.setQueryData(queryKeys.setup, { initialized: true });
  client.setQueryData(queryKeys.setupDatabase, { backend: "SQLITE" });
  client.setQueryData(queryKeys.loginBackground, { source: "STATIC", images: [] });
  client.setQueryData(queryKeys.homeContinueWatching, { items: ["previous-account"] });
  client.setQueryData(queryKeys.favorites, { items: ["previous-account"] });
  client.setQueryData(queryKeys.watchHistory, { items: ["previous-account"] });
  client.setQueryData(queryKeys.playback("item-1"), { sessionId: "previous-account" });
  client.setQueryData(["admin", "logs"], { entries: ["previous-account"] });
  return client;
}

describe("session caches", () => {
  it("keeps the session query and the pre-session server state", () => {
    const client = seededClient();
    vi.spyOn(client, "invalidateQueries").mockResolvedValue();

    clearSessionCaches(client);

    // `setup` drives the "正在启动 Lux" gate and `login-background` is the login artwork; neither
    // belongs to the account that just ended.
    expect(client.getQueryData(queryKeys.setup)).toBeDefined();
    expect(client.getQueryData(queryKeys.setupDatabase)).toBeDefined();
    expect(client.getQueryData(queryKeys.loginBackground)).toBeDefined();
    // The session query survives so its observer can report the 401.
    expect(client.getQueryData(queryKeys.me)).toBeDefined();
  });

  it("drops every account-scoped query", () => {
    const client = seededClient();
    vi.spyOn(client, "invalidateQueries").mockResolvedValue();

    clearSessionCaches(client);

    expect(client.getQueryData(queryKeys.homeContinueWatching)).toBeUndefined();
    expect(client.getQueryData(queryKeys.favorites)).toBeUndefined();
    expect(client.getQueryData(queryKeys.watchHistory)).toBeUndefined();
    expect(client.getQueryData(queryKeys.playback("item-1"))).toBeUndefined();
    expect(client.getQueryData(["admin", "logs"])).toBeUndefined();
  });

  it("re-reads the session query when a session ends", () => {
    const client = seededClient();
    const invalidate = vi.spyOn(client, "invalidateQueries").mockResolvedValue();
    clearSessionCaches(client);
    expect(invalidate).toHaveBeenCalledWith({ queryKey: queryKeys.me });
  });

  it("does not touch the session query when only the user caches are dropped", () => {
    // The 401 branch of `App` uses this: `me` already answers 401, so refetching it would repeat
    // the same request in a loop.
    const client = seededClient();
    const invalidate = vi.spyOn(client, "invalidateQueries").mockResolvedValue();

    clearUserCaches(client);

    expect(invalidate).not.toHaveBeenCalled();
    expect(client.getQueryData(queryKeys.me)).toBeDefined();
    expect(client.getQueryData(queryKeys.favorites)).toBeUndefined();
    expect(client.getQueryData(queryKeys.setup)).toBeDefined();
  });
});
