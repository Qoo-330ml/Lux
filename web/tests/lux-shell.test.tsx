// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import type { ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { LuxShell, useAvatar } from "../src/components/layout/LuxShell";
import { HomePage } from "../src/features/home/HomePage";
import { api } from "../src/lib/api/client";
import { queryKeys } from "../src/lib/api/query-keys";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

function AvatarUpdateFixture() {
  const { setAvatarUrl } = useAvatar();

  return <button type="button" onClick={() => setAvatarUrl("/api/v1/auth/avatar?v=updated")}>更新头像</button>;
}

function LocationFixture() {
  return <output data-testid="location">{useLocation().pathname}</output>;
}

type EventListener = (event: Event) => void;

class FakeEventSource {
  static instances: FakeEventSource[] = [];
  readonly listeners = new Map<string, Set<EventListener>>();
  closed = false;

  constructor(readonly url: string) {
    FakeEventSource.instances.push(this);
  }

  addEventListener(type: string, listener: EventListener) {
    const listeners = this.listeners.get(type) ?? new Set<EventListener>();
    listeners.add(listener);
    this.listeners.set(type, listeners);
  }

  removeEventListener(type: string, listener: EventListener) {
    this.listeners.get(type)?.delete(listener);
  }

  close() {
    this.closed = true;
  }

  emit(type: string, data?: string) {
    const event = new MessageEvent(type, { data });
    for (const listener of this.listeners.get(type) ?? []) listener(event);
  }
}

function renderWithProviders(root: Root, children: ReactNode) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  root.render(<QueryClientProvider client={queryClient}>{children}</QueryClientProvider>);
  return queryClient;
}

describe("LuxShell user control", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    document.title = "Lux";
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it("uses the server name as the default browser tab title", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      renderWithProviders(root,
        <MemoryRouter>
          <LuxShell
            user={{ id: "user-1", usernameNormalized: "test" }}
            serverName="客厅 Lux"
          />
        </MemoryRouter>,
      );
    });

    expect(document.title).toBe("客厅 Lux - Lux");
  });

  it("uses the built-in server title while the server name is unavailable", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      renderWithProviders(root,
        <MemoryRouter>
          <LuxShell user={{ id: "user-1", usernameNormalized: "test" }} />
        </MemoryRouter>,
      );
    });

    expect(document.title).toBe("Lux Server - Lux");
  });

  it("puts the current user id in the account route", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      renderWithProviders(root,
        <MemoryRouter initialEntries={["/"]}>
          <Routes>
            <Route element={<LuxShell user={{ id: "user-1", usernameNormalized: "test" }} />}>
              <Route index element={<LocationFixture />} />
              <Route path="account/:userId" element={<LocationFixture />} />
            </Route>
          </Routes>
        </MemoryRouter>,
      );
    });

    act(() => container.querySelector<HTMLButtonElement>(".lux-user-button")?.click());

    expect(container.querySelector("[data-testid=location]")?.textContent).toBe("/account/user-1");
  });

  it("renders the server avatar and falls back to initials when it is unavailable", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      renderWithProviders(root,
        <MemoryRouter>
          <LuxShell
            user={{
              id: "user-1",
              usernameNormalized: "test",
              displayName: "test",
            }}
          />
        </MemoryRouter>,
      );
    });

    const userButton = container.querySelector<HTMLButtonElement>(".lux-user-button");

    expect(userButton?.querySelector<HTMLImageElement>(".lux-avatar img")?.getAttribute("src")).toBe(
      "/api/v1/auth/avatar",
    );
    act(() => {
      userButton?.querySelector<HTMLImageElement>(".lux-avatar img")?.dispatchEvent(new Event("error"));
    });
    expect(userButton?.querySelector(".lux-avatar")?.textContent).toBe("T");
    expect(userButton?.querySelector(".lux-user-label")).toBeNull();
    expect(userButton?.textContent).toBe("T");
  });

  it("updates the header avatar when the account avatar changes", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      renderWithProviders(root,
        <MemoryRouter initialEntries={["/"]}>
          <Routes>
            <Route
              element={
                <LuxShell
                  user={{
                    id: "user-1",
                    usernameNormalized: "test",
                    displayName: "test",
                  }}
                />
              }
            >
              <Route index element={<AvatarUpdateFixture />} />
            </Route>
          </Routes>
        </MemoryRouter>,
      );
    });

    act(() => {
      Array.from(container.querySelectorAll<HTMLButtonElement>('button[type="button"]'))
        .find((button) => button.textContent === "更新头像")
        ?.click();
    });

    expect(container.querySelector<HTMLImageElement>(".lux-avatar img")?.getAttribute("src")).toBe(
      "/api/v1/auth/avatar?v=updated",
    );
  });

  it("renders black and white project logo variants in the brand link", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      renderWithProviders(root,
        <MemoryRouter>
          <LuxShell
            user={{
              id: "user-1",
              usernameNormalized: "test",
              displayName: "test",
            }}
          />
        </MemoryRouter>,
      );
    });

    const logo = container.querySelector<HTMLImageElement>(".lux-brand-logo");

    expect(logo?.querySelector<HTMLImageElement>(".lux-theme-logo-light")?.getAttribute("src")).toBe("/logo-black.svg");
    expect(logo?.querySelector<HTMLImageElement>(".lux-theme-logo-dark")?.getAttribute("src")).toBe("/logo-white.svg");
    expect(logo?.querySelector<HTMLImageElement>(".lux-theme-logo-light")?.getAttribute("alt")).toBe("");
  });

  it("keeps the light theme mapped to the black logo variant", () => {
    document.documentElement.dataset.luxTheme = "light";
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      renderWithProviders(root,
        <MemoryRouter>
          <LuxShell
            user={{
              id: "user-1",
              usernameNormalized: "test",
              displayName: "test",
            }}
          />
        </MemoryRouter>,
      );
    });

    const logo = container.querySelector<HTMLImageElement>(".lux-brand-logo");
    expect(logo?.querySelector(".lux-theme-logo-light")).toBeTruthy();
    expect(logo?.querySelector(".lux-theme-logo-dark")).toBeTruthy();
  });

  it("does not render duplicate search or library actions in the header", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      renderWithProviders(root,
        <MemoryRouter>
          <LuxShell
            user={{
              id: "user-1",
              usernameNormalized: "test",
              displayName: "test",
            }}
          />
        </MemoryRouter>,
      );
    });

    expect(container.querySelector('[aria-label="搜索"]')).toBeNull();
    expect(container.querySelector(".lux-grid-button")).toBeNull();
  });

  it("exposes the user's favorites in desktop and mobile navigation", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      renderWithProviders(root,
        <MemoryRouter>
          <LuxShell
            user={{
              id: "user-1",
              usernameNormalized: "test",
            }}
          />
        </MemoryRouter>,
      );
    });

    expect(container.querySelector('.lux-desktop-nav a[href="/favorites"]')?.textContent).toBe("收藏");
    act(() => container.querySelector<HTMLButtonElement>(".lux-menu-button")?.click());
    expect(container.querySelector('.lux-mobile-nav a[href="/favorites"]')?.textContent).toBe("收藏");
  });

  it("hides the back button on the home page", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      renderWithProviders(root,
        <MemoryRouter initialEntries={["/"]}>
          <LuxShell
            user={{
              id: "user-1",
              usernameNormalized: "test",
              displayName: "test",
            }}
          />
        </MemoryRouter>,
      );
    });

    expect(container.querySelector(".lux-back-button")).toBeNull();
    expect(container.querySelector(".lux-app.is-home-route")).toBeTruthy();
  });

  it("hides the back button on nested pages too", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      renderWithProviders(root,
        <MemoryRouter initialEntries={["/items/item-1"]}>
          <LuxShell
            user={{
              id: "user-1",
              usernameNormalized: "test",
              displayName: "test",
            }}
          />
        </MemoryRouter>,
      );
    });

    expect(container.querySelector(".lux-back-button")).toBeNull();
  });

  it("invalidates home and library queries when user events announce new content", () => {
    vi.stubGlobal("EventSource", FakeEventSource);
    sessionStorage.setItem("lux.home-carousel.v2:user-1", JSON.stringify({ version: 2, data: { recommended: [] } }));
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    let queryClient: QueryClient;
    act(() => {
      queryClient = renderWithProviders(root,
        <MemoryRouter>
          <LuxShell
            user={{
              id: "user-1",
              usernameNormalized: "test",
            }}
          />
        </MemoryRouter>,
      );
    });
    const invalidate = vi.spyOn(queryClient!, "invalidateQueries").mockResolvedValue();

    expect(FakeEventSource.instances[0]?.url).toBe("/api/v1/events");
    act(() => FakeEventSource.instances[0]?.emit("invalidate", JSON.stringify({ scope: "home" })));

    expect(sessionStorage.getItem("lux.home-carousel.v2:user-1")).toBeNull();
    expect(invalidate.mock.calls.map(([options]) => options)).toEqual([
      { queryKey: ["home"], refetchType: "none" },
      { queryKey: ["libraries"] },
      { queryKey: ["library"] },
    ]);
  });

  it("renders the first indexed movie and local poster before a scan completes", async () => {
    FakeEventSource.instances = [];
    sessionStorage.clear();
    vi.stubGlobal("EventSource", FakeEventSource);
    vi.spyOn(api, "homeCarousel").mockResolvedValue({ recommended: [] });
    vi.spyOn(api, "homeLibraries").mockResolvedValue({
      libraries: [{ id: "library-1", name: "电影库", kind: "MOVIE" }],
    });
    vi.spyOn(api, "homeContinueWatching").mockResolvedValue({ items: [], total: 0 });
    const firstBatch = [{
      id: "movie-1", title: "首批本地电影", itemType: "MOVIE",
      imageTags: { poster: "local-poster-v1" }, localMetadataPending: true,
    }];
    const fixture = { indexed: false, scanCompleted: false };
    const latest = vi.spyOn(api, "homeLibrariesLatest").mockImplementation(async () => ({
      libraries: [{ libraryId: "library-1", items: fixture.indexed ? firstBatch : [] }],
    }));
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const user = { id: "user-1", usernameNormalized: "test" };
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <Routes>
              <Route element={<LuxShell user={user} />}>
                <Route index element={<HomePage user={user} />} />
              </Route>
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await vi.waitFor(() => expect(latest).toHaveBeenCalledTimes(1));
    expect(container.querySelector(".lux-media-card")).toBeNull();

    // The API fixture publishes a committed batch while its scan remains running.
    fixture.indexed = true;
    act(() => FakeEventSource.instances[0]?.emit("invalidate", JSON.stringify({ scope: "home" })));
    await vi.waitFor(() => expect(latest).toHaveBeenCalledTimes(2));
    await vi.waitFor(() => expect(container.querySelector<HTMLImageElement>(
      '[aria-label="最新电影库"] .lux-media-card img',
    )?.src).toContain("tag=local-poster-v1"));
    expect(container.textContent).toContain("首批本地电影");
    expect(fixture.scanCompleted).toBe(false);
  });

  it("refreshes the library poster tags after a scraper home event", async () => {
    FakeEventSource.instances = [];
    sessionStorage.clear();
    vi.stubGlobal("EventSource", FakeEventSource);
    vi.spyOn(api, "homeCarousel").mockResolvedValue({ recommended: [] });
    vi.spyOn(api, "homeLibraries").mockResolvedValue({
      libraries: [{ id: "library-1", name: "电影库", kind: "MOVIE" }],
    });
    vi.spyOn(api, "homeContinueWatching").mockResolvedValue({ items: [], total: 0 });
    const latest = vi.spyOn(api, "homeLibrariesLatest")
      .mockResolvedValueOnce({
        libraries: [{
          libraryId: "library-1",
          items: [{ id: "movie-1", title: "刮削电影", itemType: "MOVIE", imageTags: { poster: "poster-v1" } }],
        }],
      })
      .mockResolvedValueOnce({
        libraries: [{
          libraryId: "library-1",
          items: [{ id: "movie-1", title: "刮削电影", itemType: "MOVIE", imageTags: { poster: "poster-v2" } }],
        }],
      });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const user = { id: "user-1", usernameNormalized: "test" };
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <Routes>
              <Route element={<LuxShell user={user} />}>
                <Route index element={<HomePage user={user} />} />
              </Route>
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    await vi.waitFor(() => expect(container.querySelector<HTMLImageElement>(
      '[aria-label="最新电影库"] .lux-media-card img',
    )?.src).toContain("tag=poster-v1"));
    act(() => FakeEventSource.instances[0]?.emit("invalidate", JSON.stringify({ scope: "home" })));
    await vi.waitFor(() => expect(latest).toHaveBeenCalledTimes(2));
    await vi.waitFor(() => expect(container.querySelector<HTMLImageElement>(
      '[aria-label="最新电影库"] .lux-media-card img',
    )?.src).toContain("tag=poster-v2"));
  });

  it("does not cancel a cached-home refresh when scan events arrive", async () => {
    FakeEventSource.instances = [];
    sessionStorage.clear();
    vi.stubGlobal("EventSource", FakeEventSource);
    const response = { recommended: [] };
    let resolveFirstRequest: ((value: typeof response) => void) | undefined;
    let firstSignal: AbortSignal | undefined;
    let callCount = 0;
    const homeRequest = vi.spyOn(api, "homeCarousel").mockImplementation((signal) => {
      callCount += 1;
      if (callCount > 1) return Promise.resolve(response);
      firstSignal = signal;
      return new Promise((resolve) => { resolveFirstRequest = resolve; });
    });
    vi.spyOn(api, "homeLibraries").mockResolvedValue({ libraries: [] });
    vi.spyOn(api, "homeContinueWatching").mockResolvedValue({ items: [], total: 0 });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const user = { id: "user-1", usernameNormalized: "test" };
    queryClient.setQueryData(queryKeys.homeCarousel, response);

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <Routes>
              <Route element={<LuxShell user={user} />}>
                <Route index element={<HomePage user={user} />} />
              </Route>
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
      await Promise.resolve();
    });

    expect(homeRequest).toHaveBeenCalledTimes(1);
    await act(async () => {
      FakeEventSource.instances[0]?.emit("invalidate", JSON.stringify({ scope: "home" }));
      await Promise.resolve();
    });

    expect(homeRequest).toHaveBeenCalledTimes(1);
    expect(firstSignal?.aborted).toBe(false);
    expect(queryClient.getQueryCache().find({ queryKey: queryKeys.homeCarousel })?.state.fetchStatus).toBe("fetching");

    await act(async () => {
      resolveFirstRequest?.(response);
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(homeRequest).toHaveBeenCalledTimes(2);
    expect(firstSignal?.aborted).toBe(false);
  });

  it("does not restart a failed first home load for each scan event", async () => {
    FakeEventSource.instances = [];
    sessionStorage.clear();
    vi.stubGlobal("EventSource", FakeEventSource);
    const homeRequest = vi.spyOn(api, "homeCarousel").mockRejectedValue(new Error("首页请求超时，请重试"));
    vi.spyOn(api, "homeLibraries").mockResolvedValue({ libraries: [] });
    vi.spyOn(api, "homeContinueWatching").mockResolvedValue({ items: [], total: 0 });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const user = { id: "user-1", usernameNormalized: "test" };

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <Routes>
              <Route element={<LuxShell user={user} />}>
                <Route index element={<HomePage user={user} />} />
              </Route>
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    expect(homeRequest).toHaveBeenCalledTimes(1);
    expect(container.querySelector(".lux-skeleton-page")).toBeNull();
    expect(container.querySelector('[role="status"]')?.textContent).toContain("精选轮播加载失败");

    await act(async () => {
      FakeEventSource.instances[0]?.emit("invalidate", JSON.stringify({ scope: "home" }));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(homeRequest).toHaveBeenCalledTimes(1);
    expect(container.querySelector(".lux-skeleton-page")).toBeNull();
    expect(container.querySelector('[role="status"]')?.textContent).toContain("精选轮播加载失败");
  });

  it("shows active scan progress for admins without leaking paths or query strings", async () => {
    vi.spyOn(api, "adminTaskActivity").mockResolvedValue({
      activities: [{
        id: "scan-1",
        kind: "scan",
        libraryId: "library-1",
        taskType: "RECONCILE_LIBRARY",
        status: "RUNNING",
        processedCount: 12,
        totalCount: 40,
        currentItem: "Safe.Movie.mkv",
        scanPhase: "INDEXING",
        createdAt: 1,
      }],
    });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({
      libraries: [{
        id: "library-1",
        name: "电影库",
        kind: "MOVIE",
        isEnabled: true,
        realtimeWatchEnabled: true,
        realtimeMetadataAutoMatchEnabled: false,
      }],
    });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    await act(async () => {
      renderWithProviders(root,
        <MemoryRouter>
          <LuxShell
            user={{
              id: "admin-1",
              usernameNormalized: "admin",
              canManageServer: true,
            }}
          />
        </MemoryRouter>,
      );
    });
    await vi.waitFor(() => expect(container.querySelector(".lux-scan-activity-trigger")).not.toBeNull());

    act(() => container.querySelector<HTMLButtonElement>(".lux-scan-activity-trigger")?.click());

    expect(container.textContent).toContain("电影库");
    expect(container.textContent).toContain("全量校验");
    expect(container.textContent).toContain("12/40");
    expect(container.textContent).toContain("处理文件 · Safe.Movie.mkv");
    expect(container.textContent).not.toContain("/media/");
    expect(container.textContent).not.toContain("token=");
  });
});
