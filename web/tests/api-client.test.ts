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
});
