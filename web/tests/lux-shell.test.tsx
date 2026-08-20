// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import type { ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { LuxShell, useAvatar } from "../src/components/layout/LuxShell";
import { api } from "../src/lib/api/client";

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

    expect(invalidate.mock.calls.map(([options]) => options)).toEqual([
      { queryKey: ["home"] },
      { queryKey: ["libraries"] },
      { queryKey: ["library"] },
    ]);
  });

  it("shows active scan progress for admins without leaking paths or query strings", async () => {
    vi.spyOn(api, "adminJobs").mockImplementation(async (status?: string) => ({
      jobs: status === "RUNNING" ? [{
        id: "scan-1",
        libraryId: "library-1",
        jobType: "RECONCILE_LIBRARY",
        status: "RUNNING",
        processedCount: 12,
        totalCount: 40,
        currentItem: "Safe.Movie.mkv",
        scanPhase: "INDEXING",
        createdAt: 1,
      }] : [],
    }));
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
