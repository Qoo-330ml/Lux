import { beforeEach, describe, expect, it, vi } from "vitest";
import { LuxApiClient } from "../src/lib/api/client";

describe("LuxApiClient Emby migration methods", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("uses the migration admin endpoints with bounded pagination and CSRF", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const path = String(input);
      if (path.endsWith("/test")) return new Response(JSON.stringify({ serverName: "Home Emby", historyCapability: "ITEM_STATE" }), { status: 200 });
      if (path === "/api/v1/admin/emby-migration") return new Response(JSON.stringify({ job: { id: "job-1" } }), { status: 202 });
      if (path.includes("/users?")) return new Response(JSON.stringify({ users: [], page: 1, pageSize: 50 }), { status: 200 });
      if (path.includes("/person-favorites?")) return new Response(JSON.stringify({ personFavorites: [], page: 1, pageSize: 50 }), { status: 200 });
      return new Response(JSON.stringify({}), { status: 200 });
    });
    Object.defineProperty(globalThis, "document", {
      configurable: true,
      value: { cookie: "lux_csrf=csrf-token" },
    });

    const client = new LuxApiClient();
    await expect(client.testAdminEmbyMigration()).resolves.toMatchObject({ serverName: "Home Emby" });
    const scope = { userProfile: false, libraryAccess: false, itemState: true, itemStateFilters: ["FAVORITE"] as const, personFavorites: false, targetLibraryIds: ["library-a"] };
    await expect(client.createAdminEmbyMigration({ dryRun: true, mergePolicy: "MERGE", embyUserIds: ["user-1"], scope })).resolves.toMatchObject({ job: { id: "job-1" } });
    await client.adminEmbyMigrationUsers("job-1", 2);
    await client.adminEmbyMigrationPersonFavorites("job-1", 3);

    const calls = fetchMock.mock.calls;
    expect(calls[0]?.[0]).toBe("/api/v1/admin/emby-migration/test");
    expect(JSON.parse(String(calls[0]?.[1]?.body))).toEqual({});
    expect(calls[1]?.[0]).toBe("/api/v1/admin/emby-migration");
    expect((calls[1]?.[1]?.headers as Headers).get("X-CSRF-Token")).toBe("csrf-token");
    expect(JSON.parse(String(calls[1]?.[1]?.body))).toEqual({ dryRun: true, mergePolicy: "MERGE", embyUserIds: ["user-1"], scope });
    expect(calls[2]?.[0]).toBe("/api/v1/admin/emby-migration/job-1/users?page=2&pageSize=50");
    expect(calls[3]?.[0]).toBe("/api/v1/admin/emby-migration/job-1/person-favorites?page=3&pageSize=50");
  });

  it("loads a paginated source-user list for targeted migration", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ users: [{ id: "user-1", name: "Alice", isDisabled: false, isAdministrator: false }] }), { status: 200 }),
    );
    const client = new LuxApiClient();

    await expect(client.adminEmbyMigrationSourceUsers(2)).resolves.toMatchObject({
      users: [{ id: "user-1", name: "Alice" }],
    });
    expect(fetchMock.mock.calls[0]?.[0]).toBe(
      "/api/v1/admin/emby-migration/source-users?page=2&pageSize=100",
    );
  });

  it("forwards a bounded source-user search without changing legacy calls", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ users: [], total: 0, page: 1, pageSize: 100 }), { status: 200 }),
    );
    const client = new LuxApiClient();

    await client.adminEmbyMigrationSourceUsers(1, " Alice Smith ");

    expect(fetchMock.mock.calls[0]?.[0]).toBe(
      "/api/v1/admin/emby-migration/source-users?page=1&pageSize=100&search=Alice+Smith",
    );
  });
});
