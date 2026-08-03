import { beforeEach, describe, expect, it, vi } from "vitest";
import { ApiError, LuxApiClient } from "../src/lib/api/client";

describe("LuxApiClient", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("sends same-origin JSON requests and returns the decoded payload", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ initialized: true }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      }),
    );

    const result = await new LuxApiClient().setupStatus();

    expect(result).toEqual({ initialized: true });
    const [path, options] = fetchMock.mock.calls[0] ?? [];
    expect(path).toBe("/api/v1/setup/status");
    expect(options?.credentials).toBe("same-origin");
    expect((options?.headers as Headers).get("Accept")).toBe("application/json");
  });

  it("turns the Lux error envelope into a typed error", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(
        JSON.stringify({
          error: {
            code: "AUTHENTICATION_REQUIRED",
            message: "需要登录",
            requestId: "request-1",
          },
        }),
        { status: 401, headers: { "Content-Type": "application/json" } },
      ),
    );

    await expect(new LuxApiClient().me()).rejects.toMatchObject({
      name: "ApiError",
      code: "AUTHENTICATION_REQUIRED",
      requestId: "request-1",
    } satisfies Partial<ApiError>);
  });

  it("filters library browse requests by the requested root item type", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ items: [], total: 0 }), { status: 200 }),
    );

    await new LuxApiClient().libraryItems("series-library", 1, "SERIES");

    expect(fetchMock.mock.calls[0]?.[0]).toBe(
      "/api/v1/libraries/series-library/items?page=1&pageSize=24&itemType=SERIES",
    );
  });

  it("requests the children for a series or a selected season", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ items: [], total: 0 }), { status: 200 }),
    );

    await new LuxApiClient().children("series-1", {
      itemType: "EPISODE",
      seasonId: "season-1",
    });

    expect(fetchMock.mock.calls[0]?.[0]).toBe(
      "/api/v1/items/series-1/children?page=1&pageSize=60&itemType=EPISODE&seasonId=season-1",
    );
  });

  it("exposes admin health and settings through the Lux API contract", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const path = String(input);
      if (path === "/api/v1/admin/health") {
        return new Response(JSON.stringify({ status: "ok" }), { status: 200 });
      }
      expect(path).toBe("/api/v1/admin/settings");
      expect(init?.method).toBe("PATCH");
      expect(JSON.parse(String(init?.body))).toEqual({ resumePlayedPercent: 85 });
      expect((init?.headers as Headers).get("X-CSRF-Token")).toBe("csrf-token");
      return new Response(JSON.stringify({ resumePlayedPercent: 85, resumeMinTicks: 120 }), {
        status: 200,
      });
    });
    Object.defineProperty(globalThis, "document", {
      configurable: true,
      value: { cookie: "lux_csrf=csrf-token" },
    });

    const client = new LuxApiClient();
    await expect(client.adminHealth()).resolves.toEqual({ status: "ok" });
    await expect(client.updateAdminSettings({ resumePlayedPercent: 85 })).resolves.toEqual({
      resumePlayedPercent: 85,
      resumeMinTicks: 120,
    });
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("exposes the paginated plugin catalog and installs a built-in plugin with CSRF", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const path = String(input);
      if (path === "/api/v1/admin/plugins?page=1&pageSize=50") {
        return new Response(JSON.stringify({ plugins: [{ id: "tmdb", installed: false }] }), { status: 200 });
      }
      if (path === "/api/v1/admin/plugins/tmdb/install") {
        expect(init?.method).toBe("POST");
        expect((init?.headers as Headers).get("X-CSRF-Token")).toBe("csrf-token");
        return new Response(JSON.stringify({ plugin: { id: "tmdb", installed: true } }), { status: 201 });
      }
      expect(path).toBe("/api/v1/admin/plugins/tmdb/config");
      expect(init?.method).toBe("PUT");
      expect(JSON.parse(String(init?.body))).toEqual({ apiKey: "custom-key" });
      expect((init?.headers as Headers).get("X-CSRF-Token")).toBe("csrf-token");
      return new Response(JSON.stringify({ plugin: { id: "tmdb", installed: true, configSource: "CUSTOM" } }), { status: 200 });
    });
    Object.defineProperty(globalThis, "document", {
      configurable: true,
      value: { cookie: "lux_csrf=csrf-token" },
    });

    const client = new LuxApiClient();
    await expect(client.adminPlugins()).resolves.toEqual({ plugins: [{ id: "tmdb", installed: false }] });
    await expect(client.installAdminPlugin("tmdb")).resolves.toEqual({ plugin: { id: "tmdb", installed: true } });
    await expect(client.updateAdminPluginConfig("tmdb", "custom-key")).resolves.toEqual({ plugin: { id: "tmdb", installed: true, configSource: "CUSTOM" } });
    expect(fetchMock).toHaveBeenCalledTimes(3);
  });

  it("unwraps the authenticated user from the login envelope", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ user: { id: "admin-1", canManageServer: true }, csrfToken: "csrf" }), {
        status: 200,
      }),
    );

    await expect(new LuxApiClient().login("admin", "password")).resolves.toEqual({
      id: "admin-1",
      canManageServer: true,
    });
  });

  it("unwraps the authenticated user when restoring a session", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ user: { id: "admin-1", canManageServer: true } }), { status: 200 }),
    );

    await expect(new LuxApiClient().me()).resolves.toEqual({
      id: "admin-1",
      canManageServer: true,
    });
  });
});
