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

  it("uploads an avatar as an image body instead of JSON", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ avatarUrl: "/api/v1/auth/avatar" }), { status: 200 }),
    );
    const file = new File(["avatar"], "avatar.png", { type: "image/png" });

    await expect(new LuxApiClient().uploadAvatar(file)).resolves.toEqual({
      avatarUrl: "/api/v1/auth/avatar",
    });

    const [path, options] = fetchMock.mock.calls[0] ?? [];
    expect(path).toBe("/api/v1/auth/avatar");
    expect(options?.method).toBe("PUT");
    expect(options?.body).toBe(file);
    expect((options?.headers as Headers).get("Content-Type")).toBe("image/png");
  });

  it("checks and selects the configured database backend without changing the setup API contract", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const path = String(input);
      expect(path).toMatch(/\/api\/v1\/setup\/database/);
      expect(init?.method).toBe("POST");
      expect(JSON.parse(String(init?.body))).toMatchObject({
        backend: "POSTGRESQL",
        host: "127.0.0.1",
        port: 5432,
        database: "lux",
        username: "lux",
        sslMode: "disable",
      });
      return path.endsWith("/test")
        ? new Response(JSON.stringify({ ok: true, backend: "POSTGRESQL" }), { status: 200 })
        : new Response(JSON.stringify({ selected: true, backend: "POSTGRESQL", restartRequired: true }), { status: 200 });
    });

    const input = {
      backend: "POSTGRESQL" as const,
      host: "127.0.0.1",
      port: 5432,
      database: "lux",
      username: "lux",
      password: "test-only-password",
      sslMode: "disable" as const,
    };
    await expect(new LuxApiClient().testDatabase(input)).resolves.toMatchObject({ ok: true });
    await expect(new LuxApiClient().selectDatabase(input)).resolves.toMatchObject({ restartRequired: true });
    expect(fetchMock).toHaveBeenCalledTimes(2);
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

  it("reads and updates the current user's playback threshold", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      if (!init?.method) {
        return new Response(JSON.stringify({ playedPercent: 95 }), { status: 200 });
      }
      expect(String(input)).toBe("/api/v1/auth/settings");
      expect(init.method).toBe("PATCH");
      expect(JSON.parse(String(init.body))).toEqual({ playedPercent: 80 });
      return new Response(JSON.stringify({ playedPercent: 80 }), { status: 200 });
    });
    Object.defineProperty(globalThis, "document", {
      configurable: true,
      value: { cookie: "lux_csrf=csrf-token" },
    });

    const client = new LuxApiClient();
    await expect(client.userSettings()).resolves.toEqual({ playedPercent: 95 });
    await expect(client.updateUserSettings({ playedPercent: 80 })).resolves.toEqual({ playedPercent: 80 });
    expect((fetchMock.mock.calls[1]?.[1]?.headers as Headers).get("X-CSRF-Token")).toBe("csrf-token");
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

  it("sends the selected library sort and order to the server", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ items: [], total: 0 }), { status: 200 }),
    );

    await new LuxApiClient().libraryItems("movie-library", 1, "MOVIE", {
      sortBy: "CommunityRating",
      sortOrder: "Descending",
    });

    expect(fetchMock.mock.calls[0]?.[0]).toBe(
      "/api/v1/libraries/movie-library/items?page=1&pageSize=24&itemType=MOVIE&sortBy=CommunityRating&sortOrder=Descending",
    );
  });

  it("requests only metadata items that still need confirmation", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ items: [], total: 0 }), { status: 200 }),
    );

    await new LuxApiClient().libraryItems("movie-library", 1, "MOVIE", {
      metadataStatus: "PENDING",
    });

    expect(fetchMock.mock.calls[0]?.[0]).toBe(
      "/api/v1/libraries/movie-library/items?page=1&pageSize=24&itemType=MOVIE&metadataStatus=PENDING",
    );
  });

  it("confirms selected pending metadata items in one admin request", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ confirmedCount: 2, failedCount: 0, failedItemIds: [] }), { status: 200 }),
    );

    await expect(new LuxApiClient().confirmAdminMetadata(["movie-1", "movie-2"])).resolves.toEqual({
      confirmedCount: 2,
      failedCount: 0,
      failedItemIds: [],
    });

    const [path, options] = fetchMock.mock.calls[0] ?? [];
    expect(path).toBe("/api/v1/admin/metadata/confirm");
    expect(options?.method).toBe("POST");
    expect(JSON.parse(String(options?.body))).toEqual({ itemIds: ["movie-1", "movie-2"] });
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

  it("reports a Web playback state with the shared progress endpoint", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(null, { status: 204 }),
    );
    Object.defineProperty(globalThis, "document", {
      configurable: true,
      value: { cookie: "lux_csrf=csrf-token" },
    });

    await new LuxApiClient().progress(
      "movie-1",
      1_200_000_000,
      7_200_000_000,
      "PAUSED",
      true,
    );

    const [path, options] = fetchMock.mock.calls[0] ?? [];
    expect(path).toBe("/api/v1/items/movie-1/progress");
    expect(options?.method).toBe("POST");
    expect(options?.keepalive).toBe(true);
    expect(JSON.parse(String(options?.body))).toEqual({
      positionTicks: 1_200_000_000,
      durationTicks: 7_200_000_000,
      state: "PAUSED",
    });
    expect((options?.headers as Headers).get("X-CSRF-Token")).toBe("csrf-token");
  });

  it("updates favorite and played state through the Lux item endpoints", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(null, { status: 204 }),
    );
    Object.defineProperty(globalThis, "document", {
      configurable: true,
      value: { cookie: "lux_csrf=csrf-token" },
    });

    await new LuxApiClient().setFavorite("movie-1", true);
    await new LuxApiClient().setPlayed("movie-1", false);

    expect(fetchMock.mock.calls[0]?.[0]).toBe("/api/v1/items/movie-1/favorite");
    expect(fetchMock.mock.calls[0]?.[1]?.method).toBe("PUT");
    expect(JSON.parse(String(fetchMock.mock.calls[0]?.[1]?.body))).toEqual({ favorite: true });
    expect(fetchMock.mock.calls[1]?.[0]).toBe("/api/v1/items/movie-1/played");
    expect(JSON.parse(String(fetchMock.mock.calls[1]?.[1]?.body))).toEqual({ played: false });
    expect((fetchMock.mock.calls[1]?.[1]?.headers as Headers).get("X-CSRF-Token")).toBe("csrf-token");
  });

  it("lists the current user's favorites with a bounded page", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ items: [], page: 2, pageSize: 24, total: 0 }), { status: 200 }),
    );

    await expect(new LuxApiClient().favorites(2)).resolves.toEqual({
      items: [],
      page: 2,
      pageSize: 24,
      total: 0,
    });
    expect(fetchMock.mock.calls[0]?.[0]).toBe("/api/v1/favorites?page=2&pageSize=24");
  });

  it("supports administrator candidate search and selection for identification", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const path = String(input);
      if (path === "/api/v1/admin/items/item-1/identify/candidates?page=1&pageSize=50") {
        return new Response(JSON.stringify({ items: [], total: 0 }), { status: 200 });
      }
      if (path === "/api/v1/admin/items/item-1/identify/candidates") {
        expect(init?.method).toBe("POST");
        expect(JSON.parse(String(init?.body))).toEqual({ query: "Example Movie", year: 2020 });
        return new Response(JSON.stringify({ items: [{ id: "candidate-1" }], total: 1 }), { status: 200 });
      }
      expect(path).toBe("/api/v1/admin/items/item-1/identify/candidates/candidate-1/select");
      expect(init?.method).toBe("POST");
      expect(JSON.parse(String(init?.body))).toEqual({ mode: "fillMissing" });
      return new Response(JSON.stringify({ status: "ONLINE_CONFIRMED" }), { status: 200 });
    });
    Object.defineProperty(globalThis, "document", {
      configurable: true,
      value: { cookie: "lux_csrf=csrf-token" },
    });

    const client = new LuxApiClient();
    await expect(client.adminItemCandidates("item-1")).resolves.toEqual({ items: [], total: 0 });
    await expect(client.searchAdminItemCandidates("item-1", "Example Movie", 2020)).resolves.toEqual({
      items: [{ id: "candidate-1" }],
      total: 1,
    });
    await expect(client.selectAdminMetadata("item-1", "candidate-1", "fillMissing")).resolves.toEqual({
      status: "ONLINE_CONFIRMED",
    });
    expect(fetchMock).toHaveBeenCalledTimes(3);
  });

  it("locks or unlocks every editable metadata field without changing its values", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const path = String(input);
      if (path === "/api/v1/items/item-1/metadata" && !init?.method) {
        return new Response(JSON.stringify({
          title: "标题",
          originalTitle: "Original",
          overview: "简介",
          productionYear: 2020,
          lockedFields: ["title"],
        }), { status: 200 });
      }
      expect(path).toBe("/api/v1/items/item-1/metadata");
      expect(init?.method).toBe("PATCH");
      expect(JSON.parse(String(init?.body))).toEqual({
        title: "标题",
        originalTitle: "Original",
        overview: "简介",
        productionYear: 2020,
        lockedFields: ["title", "originalTitle", "overview", "productionYear"],
      });
      return new Response(JSON.stringify({ title: "标题", lockedFields: ["title", "originalTitle", "overview", "productionYear"] }), { status: 200 });
    });

    await expect(new LuxApiClient().setItemMetadataLock("item-1", true)).resolves.toMatchObject({
      lockedFields: ["title", "originalTitle", "overview", "productionYear"],
    });
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("starts item metadata refresh and library scan jobs through the admin API", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const path = String(input);
      expect(init?.method).toBe("POST");
      if (path === "/api/v1/admin/items/item-1/metadata/refresh") {
        expect(JSON.parse(String(init?.body))).toEqual({ mode: "FILL_MISSING" });
        return new Response(JSON.stringify({ mode: "FILL_MISSING", totalCount: 1, job: { id: "metadata-job-1" } }), { status: 202 });
      }
      expect(path).toBe("/api/v1/admin/items/item-1/scan");
      return new Response(JSON.stringify({ job: { id: "scan-job-1" } }), { status: 202 });
    });

    const client = new LuxApiClient();
    await expect(client.startItemMetadataRefresh("item-1")).resolves.toEqual({ mode: "FILL_MISSING", totalCount: 1, job: { id: "metadata-job-1" } });
    await expect(client.startItemFolderScan("item-1")).resolves.toEqual({ job: { id: "scan-job-1" } });
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("starts a whole-library metadata reidentification batch", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({
        totalCount: 125,
        job: { id: "metadata-job-1", status: "QUEUED", mode: "FILL_MISSING", totalCount: 125, processedCount: 0, createdAt: 0 },
      }), { status: 202 }),
    );

    await expect(new LuxApiClient().startLibraryMetadataReidentify("library/1")).resolves.toMatchObject({
      totalCount: 125,
      job: { id: "metadata-job-1" },
    });

    const [path, options] = fetchMock.mock.calls[0] ?? [];
    expect(path).toBe("/api/v1/admin/libraries/library%2F1/reidentify");
    expect(options?.method).toBe("POST");
  });

  it("lists metadata jobs with a status filter", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ jobs: [{ id: "metadata-job-1", status: "RUNNING" }] }), { status: 200 }),
    );

    await expect(new LuxApiClient().adminMetadataReidentifyJobs("RUNNING")).resolves.toMatchObject({
      jobs: [{ id: "metadata-job-1", status: "RUNNING" }],
    });

    expect(fetchMock.mock.calls[0]?.[0]).toBe(
      "/api/v1/admin/metadata/reidentify?page=1&pageSize=50&status=RUNNING",
    );
  });

  it("loads metadata job item details from the existing job detail endpoint", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({
        job: {
          id: "metadata-job-1",
          status: "FAILED",
          mode: "FILL_MISSING",
          processedCount: 1,
          totalCount: 1,
          createdAt: 1_700_000_000,
          items: [{
            jobId: "metadata-job-1",
            itemId: "movie-1",
            status: "FAILED",
            candidateCount: 0,
            error: "TMDB_UNAVAILABLE",
            updatedAt: 1_700_000_001,
          }],
        },
      }), { status: 200 }),
    );

    const result = await new LuxApiClient().adminMetadataReidentifyJob("metadata/job 1");

    expect(fetchMock.mock.calls[0]?.[0]).toBe(
      "/api/v1/admin/metadata/reidentify/metadata%2Fjob%201",
    );
    expect(result.job.items?.[0]?.error).toBe("TMDB_UNAVAILABLE");
  });

  it("lists and updates scheduled task configurations with CSRF protection", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const path = String(input);
      if (!init?.method) {
        expect(path).toBe("/api/v1/admin/scheduled-tasks?page=1&pageSize=100");
        return new Response(JSON.stringify({ scheduledTasks: [], total: 0 }), { status: 200 });
      }
      expect(path).toBe("/api/v1/admin/scheduled-tasks");
      expect(init.method).toBe("PUT");
      expect(JSON.parse(String(init.body))).toEqual({
        ownerType: "LIBRARY",
        ownerId: "library-1",
        taskType: "RECONCILIATION_SCAN",
        schedule: "0 * * * *",
        isEnabled: true,
      });
      expect((init.headers as Headers).get("X-CSRF-Token")).toBe("csrf-token");
      return new Response(JSON.stringify({ scheduledTask: { ownerId: "library-1" } }), { status: 200 });
    });
    Object.defineProperty(globalThis, "document", {
      configurable: true,
      value: { cookie: "lux_csrf=csrf-token" },
    });

    const client = new LuxApiClient();
    await expect(client.adminScheduledTasks()).resolves.toEqual({ scheduledTasks: [], total: 0 });
    await expect(client.updateAdminScheduledTask({
      ownerType: "LIBRARY",
      ownerId: "library-1",
      taskType: "RECONCILIATION_SCAN",
      schedule: "0 * * * *",
      isEnabled: true,
    })).resolves.toEqual({ scheduledTask: { ownerId: "library-1" } });
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("downloads an administrator log archive with a bounded date query", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response("zip-bytes", {
        status: 200,
        headers: { "Content-Type": "application/zip" },
      }),
    );

    const archive = await new LuxApiClient().exportAdminLogs("2026-08-08", "2026-08-09");

    expect(await archive.text()).toBe("zip-bytes");
    const [path, options] = fetchMock.mock.calls[0] ?? [];
    expect(path).toBe("/api/v1/admin/logs/export?from=2026-08-08&to=2026-08-09");
    expect(options?.credentials).toBe("same-origin");
    expect((options?.headers as Headers).get("Accept")).toBe("application/zip");
  });

  it("downloads a single administrator log day as raw JSONL", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response('{"message":"current"}\n', {
        status: 200,
        headers: { "Content-Type": "application/x-ndjson" },
      }),
    );

    const daily = await new LuxApiClient().exportAdminLogs("2026-08-09", "2026-08-09");

    expect(await daily.text()).toContain('"message":"current"');
    const [path, options] = fetchMock.mock.calls[0] ?? [];
    expect(path).toBe("/api/v1/admin/logs/export?from=2026-08-09&to=2026-08-09");
    expect((options?.headers as Headers).get("Accept")).toBe("application/x-ndjson");
  });

  it("starts a whole-library metadata refresh with the selected mode", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({
        totalCount: 125,
        mode: "FULL_REFRESH",
        job: { id: "metadata-job-1", status: "QUEUED", mode: "FULL_REFRESH", totalCount: 125, processedCount: 0, createdAt: 0 },
      }), { status: 202 }),
    );

    await expect(new LuxApiClient().startLibraryMetadataRefresh("library/1", "FULL_REFRESH"))
      .resolves.toMatchObject({ mode: "FULL_REFRESH", totalCount: 125 });

    const [path, options] = fetchMock.mock.calls[0] ?? [];
    expect(path).toBe("/api/v1/admin/libraries/library%2F1/metadata/refresh");
    expect(options?.method).toBe("POST");
    expect(JSON.parse(String(options?.body))).toEqual({ mode: "FULL_REFRESH" });
  });

  it("queues a manual automatic library cover generation", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ status: "QUEUED", taskType: "AUTO_LIBRARY_COVER" }), { status: 202 }),
    );

    await expect(new LuxApiClient().runAutoLibraryCover("library/1"))
      .resolves.toEqual({ status: "QUEUED", taskType: "AUTO_LIBRARY_COVER" });

    const [path, options] = fetchMock.mock.calls[0] ?? [];
    expect(path).toBe("/api/v1/admin/libraries/library%2F1/cover/auto");
    expect(options?.method).toBe("POST");
  });

  it("updates the editable flags of an indexed external subtitle", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      expect(String(input)).toBe("/api/v1/admin/items/item-1/subtitles/2");
      expect(init?.method).toBe("PATCH");
      expect(JSON.parse(String(init?.body))).toEqual({
        sourceId: "source-1",
        title: "简体中文",
        language: "zho",
        isDefault: true,
        isForced: false,
      });
      return new Response(JSON.stringify({
        sourceId: "source-1",
        streamIndex: 2,
        title: "简体中文",
        language: "zho",
        isDefault: true,
        isForced: false,
      }), { status: 200 });
    });

    await expect(new LuxApiClient().updateItemSubtitle("item-1", 2, {
      sourceId: "source-1",
      title: "简体中文",
      language: "zho",
      isDefault: true,
      isForced: false,
    })).resolves.toMatchObject({ streamIndex: 2, isDefault: true });
    expect(fetchMock).toHaveBeenCalledOnce();
  });

  it("deletes the selected media source through the admin API", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response(null, { status: 204 }));

    await new LuxApiClient().deleteItem("item-1", "source-1");

    const [path, options] = fetchMock.mock.calls[0] ?? [];
    expect(path).toBe("/api/v1/admin/items/item-1?sourceId=source-1");
    expect(options?.method).toBe("DELETE");
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

  it("loads the dashboard aggregate and persists a server name", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const path = String(input);
      if (path === "/api/v1/admin/dashboard") {
        return new Response(JSON.stringify({ server: { name: "Lux", version: "0.2.7" }, health: {}, nowPlaying: [], activity: [] }), { status: 200 });
      }
      expect(path).toBe("/api/v1/admin/settings");
      expect(init?.method).toBe("PATCH");
      expect(JSON.parse(String(init?.body))).toEqual({ serverName: "客厅 Lux" });
      expect((init?.headers as Headers).get("X-CSRF-Token")).toBe("csrf-token");
      return new Response(JSON.stringify({ serverName: "客厅 Lux" }), { status: 200 });
    });
    Object.defineProperty(globalThis, "document", {
      configurable: true,
      value: { cookie: "lux_csrf=csrf-token" },
    });

    const client = new LuxApiClient();
    await expect(client.adminDashboard()).resolves.toMatchObject({
      server: { name: "Lux", version: "0.2.7" },
      nowPlaying: [],
      activity: [],
    });
    await expect(client.updateAdminSettings({ serverName: "客厅 Lux" })).resolves.toEqual({ serverName: "客厅 Lux" });
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("tests the fixed network proxy targets with the configured CSRF token", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      expect(String(input)).toBe("/api/v1/admin/settings/network-proxy/test");
      expect(init?.method).toBe("POST");
      expect(JSON.parse(String(init?.body))).toEqual({ networkProxyUrl: "http://192.168.1.2:7890" });
      expect((init?.headers as Headers).get("X-CSRF-Token")).toBe("csrf-token");
      return new Response(JSON.stringify({ probes: [], egressIp: null, egressCountry: null }), { status: 200 });
    });
    Object.defineProperty(globalThis, "document", {
      configurable: true,
      value: { cookie: "lux_csrf=csrf-token" },
    });

    await expect(new LuxApiClient().testAdminNetworkProxy("http://192.168.1.2:7890")).resolves.toMatchObject({
      probes: [],
      egressIp: null,
      egressCountry: null,
    });
    expect(fetchMock).toHaveBeenCalledOnce();
  });

  it("exposes the paginated plugin catalog and installs a store plugin with CSRF", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const path = String(input);
      if (path === "/api/v1/admin/plugins?page=1&pageSize=50") {
        return new Response(JSON.stringify({ plugins: [{ id: "tmdb", installed: false }] }), { status: 200 });
      }
      if (path === "/api/v1/admin/plugins/installed?page=1&pageSize=50") {
        return new Response(JSON.stringify({ plugins: [{ id: "tmdb", installed: true }] }), { status: 200 });
      }
      if (path === "/api/v1/admin/plugins/tmdb/install") {
        expect(init?.method).toBe("POST");
        expect((init?.headers as Headers).get("X-CSRF-Token")).toBe("csrf-token");
        return new Response(JSON.stringify({ plugin: { id: "tmdb", installed: true } }), { status: 201 });
      }
      if (path === "/api/v1/admin/plugins/tmdb/update") {
        expect(init?.method).toBe("POST");
        expect((init?.headers as Headers).get("X-CSRF-Token")).toBe("csrf-token");
        return new Response(JSON.stringify({ plugin: { id: "tmdb", installed: true } }), { status: 200 });
      }
      if (path === "/api/v1/admin/plugins/tmdb/enabled") {
        expect(init?.method).toBe("PATCH");
        expect(JSON.parse(String(init?.body))).toEqual({ enabled: false });
        expect((init?.headers as Headers).get("X-CSRF-Token")).toBe("csrf-token");
        return new Response(JSON.stringify({ plugin: { id: "tmdb", installed: true, enabled: false } }), { status: 200 });
      }
      if (path === "/api/v1/admin/plugins/tmdb" && init?.method === "DELETE") {
        expect((init?.headers as Headers).get("X-CSRF-Token")).toBe("csrf-token");
        return new Response(null, { status: 204 });
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
    await expect(client.adminInstalledPlugins()).resolves.toEqual({ plugins: [{ id: "tmdb", installed: true }] });
    await expect(client.installAdminPlugin("tmdb")).resolves.toEqual({ plugin: { id: "tmdb", installed: true } });
    await expect(client.updateAdminPlugin("tmdb")).resolves.toEqual({ plugin: { id: "tmdb", installed: true } });
    await expect(client.updateAdminPluginEnabled("tmdb", false)).resolves.toEqual({ plugin: { id: "tmdb", installed: true, enabled: false } });
    await expect(client.uninstallAdminPlugin("tmdb")).resolves.toBeUndefined();
    await expect(client.updateAdminPluginConfig("tmdb", "custom-key")).resolves.toEqual({ plugin: { id: "tmdb", installed: true, configSource: "CUSTOM" } });
    expect(fetchMock).toHaveBeenCalledTimes(7);
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

  it("sends non-sensitive TMDb language and API address settings without requiring an API key", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ plugin: { id: "tmdb" } }), { status: 200 }),
    );
    Object.defineProperty(globalThis, "document", {
      configurable: true,
      value: { cookie: "lux_csrf=csrf-token" },
    });

    await new LuxApiClient().updateAdminPluginConfig("tmdb", {
      preferredLanguage: "zh-CN",
      languageFallbackEnabled: false,
      fallbackLanguages: ["zh-SG", "zh-HK", "zh-TW"],
      alternateApiEnabled: true,
      apiBaseUrl: "https://api.tmdb.org",
    });

    expect(JSON.parse(String(fetchMock.mock.calls[0]?.[1]?.body))).toEqual({
      preferredLanguage: "zh-CN",
      languageFallbackEnabled: false,
      fallbackLanguages: ["zh-SG", "zh-HK", "zh-TW"],
      alternateApiEnabled: true,
      apiBaseUrl: "https://api.tmdb.org",
    });
  });

  it("saves media-info configuration and starts the configured plugin", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const path = String(input);
      if (path.endsWith("/config")) {
        expect(init?.method).toBe("PUT");
        expect(JSON.parse(String(init?.body))).toEqual({
          libraryIds: ["library-1"],
          concurrency: 3,
          existingInfoPolicy: "SKIP",
          writeSidecars: true,
          schedule: "0 3 * * *",
        });
        return new Response(JSON.stringify({ plugin: { id: "org.lux.strm-media-info" } }), { status: 200 });
      }
      expect(path).toBe("/api/v1/admin/plugins/org.lux.strm-media-info/run");
      expect(init?.method).toBe("POST");
      return new Response(JSON.stringify({ operationId: "operation-1", jobs: [] }), { status: 202 });
    });
    Object.defineProperty(globalThis, "document", {
      configurable: true,
      value: { cookie: "lux_csrf=csrf-token" },
    });

    const client = new LuxApiClient();
    await expect(client.updateAdminPluginConfig("org.lux.strm-media-info", {
      libraryIds: ["library-1"],
      concurrency: 3,
      existingInfoPolicy: "SKIP",
      writeSidecars: true,
      schedule: "0 3 * * *",
    })).resolves.toEqual({ plugin: { id: "org.lux.strm-media-info" } });
    await expect(client.runAdminPlugin("org.lux.strm-media-info")).resolves.toEqual({
      operationId: "operation-1",
      jobs: [],
    });
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("returns the authenticated user and server name when restoring a session", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ user: { id: "admin-1", canManageServer: true }, serverName: "客厅 Lux" }), { status: 200 }),
    );

    await expect(new LuxApiClient().me()).resolves.toEqual({
      user: {
        id: "admin-1",
        canManageServer: true,
      },
      serverName: "客厅 Lux",
    });
  });
});
