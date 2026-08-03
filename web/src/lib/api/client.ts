import type {
  AdminAuditEvent,
  AdminHealth,
  AdminImage,
  AdminJob,
  AdminLibrary,
  AdminRoot,
  AdminSettings,
  AdminUser,
  AdminMetadataCandidate,
  ApiErrorBody,
  HomeResponse,
  Library,
  LuxUser,
  MediaItem,
  PageResponse,
  PlaybackState,
  SetupStatus,
} from "./types";

const csrfCookie = "lux_csrf";

export class ApiError extends Error {
  readonly code: string;
  readonly requestId?: string;
  readonly status: number;

  constructor(
    message: string,
    options: { code?: string; requestId?: string; status: number },
  ) {
    super(message);
    this.name = "ApiError";
    this.code = options.code ?? "UNKNOWN";
    this.requestId = options.requestId;
    this.status = options.status;
  }
}

function readCookie(name: string): string {
  if (typeof document === "undefined") return "";
  const value = document.cookie
    .split("; ")
    .find((part) => part.startsWith(`${name}=`));
  return value ? decodeURIComponent(value.slice(name.length + 1)) : "";
}

async function readJson<T>(response: Response): Promise<T | undefined> {
  if (response.status === 204) return undefined;
  return (await response.json().catch(() => undefined)) as T | undefined;
}

export class LuxApiClient {
  async request<T>(path: string, options: RequestInit = {}): Promise<T> {
    const method = options.method?.toUpperCase() ?? "GET";
    const headers = new Headers(options.headers);
    headers.set("Accept", "application/json");
    if (options.body && !headers.has("Content-Type")) {
      headers.set("Content-Type", "application/json");
    }
    if (method !== "GET" && method !== "HEAD") {
      const csrf = readCookie(csrfCookie);
      if (csrf) headers.set("X-CSRF-Token", csrf);
    }

    const response = await fetch(path, {
      ...options,
      credentials: "same-origin",
      headers,
    });
    const body = await readJson<T & ApiErrorBody>(response);
    if (!response.ok) {
      throw new ApiError(
        body && "error" in body
          ? body.error?.message ?? "请求失败"
          : "请求失败",
        {
          code: body && "error" in body ? body.error?.code : undefined,
          requestId: body && "error" in body ? body.error?.requestId : undefined,
          status: response.status,
        },
      );
    }
    return body as T;
  }

  setupStatus() {
    return this.request<SetupStatus>("/api/v1/setup/status");
  }

  setup(input: {
    username: string;
    displayName?: string;
    password: string;
    tmdbToken?: string;
    libraryName?: string;
    libraryKind?: string;
    libraryRoot?: string;
  }) {
    return this.request<{ user: LuxUser }>("/api/v1/setup/complete", {
      method: "POST",
      body: JSON.stringify(input),
    });
  }

  login(username: string, password: string) {
    return this.request<{ user: LuxUser; csrfToken?: string }>("/api/v1/auth/login", {
      method: "POST",
      body: JSON.stringify({ username, password }),
    }).then((response) => response.user);
  }

  logout() {
    return this.request<void>("/api/v1/auth/logout", { method: "POST" });
  }

  me() {
    return this.request<{ user: LuxUser }>("/api/v1/auth/me").then((response) => response.user);
  }

  home() {
    return this.request<HomeResponse>("/api/v1/home");
  }

  libraries() {
    return this.request<{ libraries?: Library[] }>("/api/v1/libraries");
  }

  libraryItems(libraryId: string, page = 1) {
    const params = new URLSearchParams({ page: String(page), pageSize: "24" });
    return this.request<PageResponse<MediaItem>>(
      `/api/v1/libraries/${encodeURIComponent(libraryId)}/items?${params}`,
    );
  }

  search(query: string, page = 1) {
    const params = new URLSearchParams({ q: query, page: String(page), pageSize: "24" });
    return this.request<PageResponse<MediaItem>>(`/api/v1/search?${params}`);
  }

  item(itemId: string) {
    return this.request<MediaItem>(`/api/v1/items/${encodeURIComponent(itemId)}`);
  }

  playback(itemId: string) {
    return this.request<PlaybackState>(
      `/api/v1/items/${encodeURIComponent(itemId)}/playback`,
    );
  }

  adminHealth() {
    return this.request<AdminHealth>("/api/v1/admin/health");
  }

  adminLibraries() {
    return this.request<{ libraries?: AdminLibrary[] }>("/api/v1/admin/libraries");
  }

  createAdminLibrary(input: { name: string; kind: string; realtimeWatchEnabled: boolean }) {
    return this.request<{ library: AdminLibrary }>("/api/v1/admin/libraries", {
      method: "POST",
      body: JSON.stringify(input),
    });
  }

  updateAdminLibrary(libraryId: string, input: Record<string, unknown>) {
    return this.request<{ library: AdminLibrary }>(
      `/api/v1/admin/libraries/${encodeURIComponent(libraryId)}`,
      { method: "PATCH", body: JSON.stringify(input) },
    );
  }

  deleteAdminLibrary(libraryId: string) {
    return this.request<void>(`/api/v1/admin/libraries/${encodeURIComponent(libraryId)}`, {
      method: "DELETE",
    });
  }

  addAdminLibraryRoot(libraryId: string, path: string) {
    return this.request<{ root: AdminRoot; warnings?: string[] }>(
      `/api/v1/admin/libraries/${encodeURIComponent(libraryId)}/roots`,
      { method: "POST", body: JSON.stringify({ path }) },
    );
  }

  deleteAdminLibraryRoot(libraryId: string, rootId: string) {
    return this.request<void>(
      `/api/v1/admin/libraries/${encodeURIComponent(libraryId)}/roots/${encodeURIComponent(rootId)}`,
      { method: "DELETE" },
    );
  }

  startAdminScan(libraryId: string) {
    return this.request<{ job: AdminJob }>(
      `/api/v1/admin/libraries/${encodeURIComponent(libraryId)}/scan`,
      { method: "POST" },
    );
  }

  adminUsers() {
    return this.request<{ users?: AdminUser[] }>("/api/v1/admin/users");
  }

  createAdminUser(input: {
    username: string;
    displayName: string;
    password: string;
    isAdmin: boolean;
  }) {
    return this.request<{ user: AdminUser }>("/api/v1/admin/users", {
      method: "POST",
      body: JSON.stringify(input),
    });
  }

  updateAdminUser(userId: string, input: Record<string, unknown>) {
    return this.request<{ user: AdminUser }>(
      `/api/v1/admin/users/${encodeURIComponent(userId)}`,
      { method: "PATCH", body: JSON.stringify(input) },
    );
  }

  disableAdminUser(userId: string) {
    return this.request<{ user: AdminUser }>(
      `/api/v1/admin/users/${encodeURIComponent(userId)}`,
      { method: "DELETE" },
    );
  }

  adminUserLibraryAccess(userId: string) {
    return this.request<{ libraryIds?: string[] }>(
      `/api/v1/admin/users/${encodeURIComponent(userId)}/libraries`,
    );
  }

  setAdminUserLibraryAccess(userId: string, libraryId: string, canView: boolean) {
    return this.request<{ canView: boolean }>(
      `/api/v1/admin/users/${encodeURIComponent(userId)}/libraries/${encodeURIComponent(libraryId)}`,
      { method: "PATCH", body: JSON.stringify({ canView }) },
    );
  }

  adminJobs(status?: string) {
    const params = new URLSearchParams({ page: "1", pageSize: "50" });
    if (status) params.set("status", status);
    return this.request<{ jobs?: AdminJob[] }>(`/api/v1/admin/jobs?${params}`);
  }

  cancelAdminJob(jobId: string) {
    return this.request<void>(`/api/v1/admin/jobs/${encodeURIComponent(jobId)}/cancel`, {
      method: "POST",
    });
  }

  retryAdminJob(jobId: string) {
    return this.request<{ job: AdminJob }>(`/api/v1/admin/jobs/${encodeURIComponent(jobId)}/retry`, {
      method: "POST",
    });
  }

  adminLogs() {
    return this.request<{ events?: AdminAuditEvent[] }>(
      "/api/v1/admin/logs?page=1&pageSize=50",
    );
  }

  adminSettings() {
    return this.request<AdminSettings>("/api/v1/admin/settings");
  }

  updateAdminSettings(input: Partial<AdminSettings>) {
    return this.request<AdminSettings>("/api/v1/admin/settings", {
      method: "PATCH",
      body: JSON.stringify(input),
    });
  }

  adminPendingMetadata() {
    return this.request<{ items?: AdminMetadataCandidate[]; total?: number }>(
      "/api/v1/admin/metadata/pending?page=1&pageSize=50",
    );
  }

  selectAdminMetadata(itemId: string, candidateId: string, mode: "fillMissing" | "refreshUnlocked") {
    return this.request<{ itemId: string; candidateId: string; status: string }>(
      `/api/v1/admin/items/${encodeURIComponent(itemId)}/identify/candidates/${encodeURIComponent(candidateId)}/select`,
      { method: "POST", body: JSON.stringify({ mode }) },
    );
  }

  adminItemImages(itemId: string) {
    return this.request<{ images?: AdminImage[] }>(
      `/api/v1/admin/items/${encodeURIComponent(itemId)}/images`,
    );
  }

  deleteAdminItemImage(itemId: string, imageId: string) {
    return this.request<void>(
      `/api/v1/admin/items/${encodeURIComponent(itemId)}/images/${encodeURIComponent(imageId)}`,
      { method: "DELETE" },
    );
  }
}

export const api = new LuxApiClient();
