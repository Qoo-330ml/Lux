import type {
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
    return this.request<LuxUser>("/api/v1/auth/login", {
      method: "POST",
      body: JSON.stringify({ username, password }),
    });
  }

  logout() {
    return this.request<void>("/api/v1/auth/logout", { method: "POST" });
  }

  me() {
    return this.request<LuxUser>("/api/v1/auth/me");
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
}

export const api = new LuxApiClient();
