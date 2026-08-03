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
});
