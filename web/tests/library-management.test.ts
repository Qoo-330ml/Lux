import { beforeEach, describe, expect, it, vi } from "vitest";
import { LuxApiClient } from "../src/lib/api/client";

describe("library management API", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    Object.defineProperty(globalThis, "document", {
      configurable: true,
      value: { cookie: "lux_csrf=csrf-token" },
    });
  });

  it("sends library name and type edits through the admin patch endpoint", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ library: { id: "library-1", name: "剧集", kind: "SERIES" } }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      }),
    );

    await new LuxApiClient().updateAdminLibrary("library-1", { name: "剧集", kind: "SERIES" });

    const [path, options] = fetchMock.mock.calls[0] ?? [];
    expect(path).toBe("/api/v1/admin/libraries/library-1");
    expect(options?.method).toBe("PATCH");
    expect(JSON.parse(String(options?.body))).toEqual({ name: "剧集", kind: "SERIES" });
    expect((options?.headers as Headers).get("X-CSRF-Token")).toBe("csrf-token");
  });

  it("uploads the selected cover with its image content type", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ library: { id: "library-1" } }), { status: 200 }),
    );
    const cover = new Blob(["image"], { type: "image/png" });

    await new LuxApiClient().updateAdminLibraryCover("library-1", cover);

    const [path, options] = fetchMock.mock.calls[0] ?? [];
    expect(path).toBe("/api/v1/admin/libraries/library-1/cover");
    expect(options?.method).toBe("PUT");
    expect((options?.headers as Headers).get("Content-Type")).toBe("image/png");
    expect(options?.body).toBe(cover);
    expect((options?.headers as Headers).get("X-CSRF-Token")).toBe("csrf-token");
  });
});
