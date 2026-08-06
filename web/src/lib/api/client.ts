import type {
  AdminAuditEvent,
  AdminDashboard,
  AdminHealth,
  AdminImage,
  AdminJob,
  AdminScheduledTask,
  AdminScheduledTaskPage,
  AdminMetadataReidentifyJob,
  AdminLibrary,
  AdminRoot,
  AdminSettings,
  AdminSettingsPatch,
  NetworkProxyDiagnostics,
  AdminUser,
  AdminMetadataCandidate,
  AdminMetadataReidentifyStart,
  AdminPlugin,
  ApiErrorBody,
  HomeResponse,
  Library,
  LuxUser,
  MediaItem,
  ItemMetadata,
  ItemImage,
  ImageSearchResult,
  MetadataFieldName,
  PageResponse,
  PlaybackState,
  PlaybackEventState,
  SetupStatus,
  MetadataRefreshMode,
} from "./types";

const csrfCookie = "lux_csrf";

export type LibrarySortBy = "Name" | "DateCreated" | "PremiereDate" | "CommunityRating";
export type LibrarySortOrder = "Ascending" | "Descending";
export type LibraryItemsOptions = {
  sortBy?: LibrarySortBy;
  sortOrder?: LibrarySortOrder;
};

export type AdminDirectoryEntry = {
  name: string;
  path: string;
};

export type AdminDirectoryPage = {
  path: string;
  parentPath: string | null;
  directories: AdminDirectoryEntry[];
  page: number;
  pageSize: number;
  hasMore: boolean;
};

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

  libraryItems(
    libraryId: string,
    page = 1,
    itemTypes?: string,
    options: LibraryItemsOptions = {},
  ) {
    const params = new URLSearchParams({ page: String(page), pageSize: "24" });
    if (itemTypes) params.set("itemType", itemTypes);
    if (options.sortBy) params.set("sortBy", options.sortBy);
    if (options.sortOrder) params.set("sortOrder", options.sortOrder);
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

  itemMetadata(itemId: string) {
    return this.request<ItemMetadata>(`/api/v1/items/${encodeURIComponent(itemId)}/metadata`);
  }

  updateItemMetadata(
    itemId: string,
    input: {
      title: string;
      originalTitle?: string;
      overview?: string;
      productionYear?: number;
      lockedFields: MetadataFieldName[];
    },
  ) {
    return this.request<ItemMetadata>(`/api/v1/items/${encodeURIComponent(itemId)}/metadata`, {
      method: "PATCH",
      body: JSON.stringify(input),
    });
  }

  async setItemMetadataLock(itemId: string, locked: boolean) {
    const metadata = await this.itemMetadata(itemId);
    return this.updateItemMetadata(itemId, {
      title: metadata.title,
      originalTitle: metadata.originalTitle ?? undefined,
      overview: metadata.overview ?? undefined,
      productionYear: metadata.productionYear ?? undefined,
      lockedFields: locked ? ["title", "originalTitle", "overview", "productionYear"] : [],
    });
  }

  startItemMetadataRefresh(itemId: string) {
    return this.request<AdminMetadataReidentifyStart>(
      `/api/v1/admin/items/${encodeURIComponent(itemId)}/metadata/refresh`,
      {
      method: "POST",
        body: JSON.stringify({ mode: "FILL_MISSING" }),
      },
    );
  }

  startItemLibraryScan(itemId: string) {
    return this.request<{ job: AdminJob }>(
      `/api/v1/admin/items/${encodeURIComponent(itemId)}/scan`,
      { method: "POST" },
    );
  }

  updateItemSubtitle(
    itemId: string,
    streamIndex: number,
    input: {
      sourceId: string;
      title?: string;
      language?: string;
      isDefault: boolean;
      isForced: boolean;
    },
  ) {
    return this.request<{
      sourceId: string;
      streamIndex: number;
      title?: string | null;
      language?: string | null;
      isDefault: boolean;
      isForced: boolean;
    }>(`/api/v1/admin/items/${encodeURIComponent(itemId)}/subtitles/${streamIndex}`, {
      method: "PATCH",
      body: JSON.stringify(input),
    });
  }

  deleteItem(itemId: string, sourceId?: string) {
    const query = sourceId ? `?sourceId=${encodeURIComponent(sourceId)}` : "";
    return this.request<void>(`/api/v1/admin/items/${encodeURIComponent(itemId)}${query}`, {
      method: "DELETE",
    });
  }

  itemImages(itemId: string) {
    return this.request<{ images?: ItemImage[] }>(`/api/v1/items/${encodeURIComponent(itemId)}/images`);
  }

  searchItemImages(itemId: string, input: { imageType: string; language: string; source: string }) {
    return this.request<{ images?: ImageSearchResult[] }>(
      `/api/v1/items/${encodeURIComponent(itemId)}/images/search`,
      { method: "POST", body: JSON.stringify(input) },
    );
  }

  selectItemImage(itemId: string, input: { imageType: string; url: string; language?: string | null }) {
    return this.request<{ image: ItemImage }>(
      `/api/v1/items/${encodeURIComponent(itemId)}/images/select`,
      { method: "POST", body: JSON.stringify(input) },
    );
  }

  children(itemId: string, options: { itemType?: string; seasonId?: string } = {}) {
    const params = new URLSearchParams({ page: "1", pageSize: "60" });
    if (options.itemType) params.set("itemType", options.itemType);
    if (options.seasonId) params.set("seasonId", options.seasonId);
    return this.request<PageResponse<MediaItem>>(
      `/api/v1/items/${encodeURIComponent(itemId)}/children?${params}`,
    );
  }

  playback(itemId: string) {
    return this.request<PlaybackState>(
      `/api/v1/items/${encodeURIComponent(itemId)}/playback`,
    );
  }

  progress(
    itemId: string,
    positionTicks: number,
    durationTicks: number | null,
    state: PlaybackEventState = "PLAYING",
    keepalive = false,
  ) {
    return this.request<void>(`/api/v1/items/${encodeURIComponent(itemId)}/progress`, {
      method: "POST",
      keepalive,
      body: JSON.stringify({ positionTicks, durationTicks, state }),
    });
  }

  adminHealth() {
    return this.request<AdminHealth>("/api/v1/admin/health");
  }

  adminDashboard() {
    return this.request<AdminDashboard>("/api/v1/admin/dashboard");
  }

  adminLibraries() {
    return this.request<{ libraries?: AdminLibrary[] }>("/api/v1/admin/libraries");
  }

  adminPlugins() {
    return this.request<{ plugins?: AdminPlugin[]; total?: number; page?: number; pageSize?: number }>(
      "/api/v1/admin/plugins?page=1&pageSize=50",
    );
  }

  adminInstalledPlugins() {
    return this.request<{ plugins?: AdminPlugin[]; total?: number; page?: number; pageSize?: number }>(
      "/api/v1/admin/plugins/installed?page=1&pageSize=50",
    );
  }

  installAdminPlugin(pluginId: string) {
    return this.request<{ plugin: AdminPlugin }>(
      `/api/v1/admin/plugins/${encodeURIComponent(pluginId)}/install`,
      { method: "POST" },
    );
  }

  updateAdminPluginConfig(
    pluginId: string,
    input: string | Record<string, unknown>,
  ) {
    const body = typeof input === "string" ? { apiKey: input } : input;
    return this.request<{ plugin: AdminPlugin }>(
      `/api/v1/admin/plugins/${encodeURIComponent(pluginId)}/config`,
      { method: "PUT", body: JSON.stringify(body) },
    );
  }

  runAdminPlugin(pluginId: string) {
    return this.request<{ operationId: string; jobs: Array<Record<string, unknown>> }>(
      `/api/v1/admin/plugins/${encodeURIComponent(pluginId)}/run`,
      { method: "POST" },
    );
  }

  createAdminLibrary(input: { name: string; kind: string; scraperId?: string | null }) {
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

  updateAdminLibraryCover(libraryId: string, file: Blob) {
    return this.request<{ library: AdminLibrary }>(
      `/api/v1/admin/libraries/${encodeURIComponent(libraryId)}/cover`,
      {
        method: "PUT",
        headers: { "Content-Type": file.type || "application/octet-stream" },
        body: file,
      },
    );
  }

  deleteAdminLibrary(libraryId: string) {
    return this.request<void>(`/api/v1/admin/libraries/${encodeURIComponent(libraryId)}`, {
      method: "DELETE",
    });
  }

  addAdminLibraryRoot(libraryId: string, path: string) {
    return this.request<{ root: AdminRoot; warnings?: string[]; scanJob?: AdminJob }>(
      `/api/v1/admin/libraries/${encodeURIComponent(libraryId)}/roots`,
      { method: "POST", body: JSON.stringify({ path }) },
    );
  }

  adminDirectories(path = "/", page = 1, pageSize = 50) {
    const params = new URLSearchParams({ path, page: String(page), pageSize: String(pageSize) });
    return this.request<AdminDirectoryPage>(`/api/v1/admin/directories?${params}`);
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

  startLibraryMetadataReidentify(libraryId: string) {
    return this.request<AdminMetadataReidentifyStart>(
      `/api/v1/admin/libraries/${encodeURIComponent(libraryId)}/reidentify`,
      { method: "POST" },
    );
  }

  startLibraryMetadataRefresh(libraryId: string, mode: MetadataRefreshMode) {
    return this.request<AdminMetadataReidentifyStart>(
      `/api/v1/admin/libraries/${encodeURIComponent(libraryId)}/metadata/refresh`,
      { method: "POST", body: JSON.stringify({ mode }) },
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

  adminScheduledTasks(page = 1) {
    return this.request<AdminScheduledTaskPage>(
      `/api/v1/admin/scheduled-tasks?page=${page}&pageSize=100`,
    );
  }

  updateAdminScheduledTask(input: {
    ownerType: "LIBRARY";
    ownerId: string;
    taskType: string;
    schedule: string | null;
    isEnabled?: boolean;
  }) {
    return this.request<{ scheduledTask: AdminScheduledTask }>(
      "/api/v1/admin/scheduled-tasks",
      { method: "PUT", body: JSON.stringify(input) },
    );
  }

  adminMetadataReidentifyJobs(status?: string) {
    const params = new URLSearchParams({ page: "1", pageSize: "50" });
    if (status) params.set("status", status);
    return this.request<{ jobs?: AdminMetadataReidentifyJob[] }>(
      `/api/v1/admin/metadata/reidentify?${params}`,
    );
  }

  retryMetadataReidentify(jobId: string) {
    return this.request<{ job: AdminMetadataReidentifyJob }>(
      `/api/v1/admin/metadata/reidentify/${encodeURIComponent(jobId)}`,
      { method: "POST" },
    );
  }

  cancelMetadataReidentify(jobId: string) {
    return this.request<void>(
      `/api/v1/admin/metadata/reidentify/${encodeURIComponent(jobId)}/cancel`,
      { method: "POST" },
    );
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

  updateAdminSettings(input: AdminSettingsPatch) {
    return this.request<AdminSettings>("/api/v1/admin/settings", {
      method: "PATCH",
      body: JSON.stringify(input),
    });
  }

  testAdminNetworkProxy(networkProxyUrl?: string) {
    return this.request<NetworkProxyDiagnostics>(
      "/api/v1/admin/settings/network-proxy/test",
      {
        method: "POST",
        body: JSON.stringify(networkProxyUrl ? { networkProxyUrl } : {}),
      },
    );
  }

  adminPendingMetadata() {
    return this.request<{ items?: AdminMetadataCandidate[]; total?: number }>(
      "/api/v1/admin/metadata/pending?page=1&pageSize=50",
    );
  }

  adminItemCandidates(itemId: string) {
    return this.request<{ items?: AdminMetadataCandidate[]; total?: number }>(
      `/api/v1/admin/items/${encodeURIComponent(itemId)}/identify/candidates?page=1&pageSize=50`,
    );
  }

  searchAdminItemCandidates(itemId: string, query: string, year?: number) {
    return this.request<{ items?: AdminMetadataCandidate[]; total?: number }>(
      `/api/v1/admin/items/${encodeURIComponent(itemId)}/identify/candidates`,
      { method: "POST", body: JSON.stringify({ query, year }) },
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
