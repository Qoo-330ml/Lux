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
      return new Response(JSON.stringify({}), { status: 200 });
    });
    Object.defineProperty(globalThis, "document", {
      configurable: true,
      value: { cookie: "lux_csrf=csrf-token" },
    });

    const client = new LuxApiClient();
    const source = { baseUrl: "http://emby.example.test:8096", apiKey: "secret", allowPrivateNetwork: false };
    await expect(client.testAdminEmbyMigration(source)).resolves.toMatchObject({ serverName: "Home Emby" });
    await expect(client.createAdminEmbyMigration({ source, dryRun: true, mergePolicy: "MERGE" })).resolves.toMatchObject({ job: { id: "job-1" } });
    await client.adminEmbyMigrationUsers("job-1", 2);

    const calls = fetchMock.mock.calls;
    expect(calls[0]?.[0]).toBe("/api/v1/admin/emby-migration/test");
    expect(JSON.parse(String(calls[0]?.[1]?.body))).toEqual(source);
    expect(calls[1]?.[0]).toBe("/api/v1/admin/emby-migration");
    expect((calls[1]?.[1]?.headers as Headers).get("X-CSRF-Token")).toBe("csrf-token");
    expect(JSON.parse(String(calls[1]?.[1]?.body))).toEqual({ source, dryRun: true, mergePolicy: "MERGE" });
    expect(calls[2]?.[0]).toBe("/api/v1/admin/emby-migration/job-1/users?page=2&pageSize=50");
  });
});
