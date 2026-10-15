// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { SseConnectionContext, type SseConnectionState } from "../src/features/activity/useUserEvents";
import { MediaDetailPage } from "../src/features/detail/MediaDetailPage";
import { HomePage } from "../src/features/home/HomePage";
import { api } from "../src/lib/api/client";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let container: HTMLDivElement;
let root: Root;
let client: QueryClient;

beforeEach(() => {
  vi.useFakeTimers();
  localStorage.clear();
  sessionStorage.clear();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: 60_000 } } });
  vi.spyOn(api, "homeCarousel").mockResolvedValue({ recommended: [] });
  vi.spyOn(api, "homeLibraries").mockResolvedValue({ libraries: [{ id: "library-1", name: "电影库", kind: "MOVIE" }] });
  vi.spyOn(api, "homeContinueWatching").mockResolvedValue({ items: [], total: 0 });
  vi.spyOn(api, "homeLibrariesLatest").mockResolvedValue({ libraries: [{ libraryId: "library-1", items: [] }] });
  vi.spyOn(api, "item").mockResolvedValue({ id: "movie-1", title: "电影", itemType: "MOVIE" });
  vi.spyOn(api, "itemImages").mockResolvedValue({ images: [] });
  vi.spyOn(api, "playback").mockResolvedValue({ mediaSources: [] });
});

afterEach(() => {
  act(() => root.unmount());
  client.clear();
  container.remove();
  vi.restoreAllMocks();
  vi.useRealTimers();
});

async function advance(ms: number) {
  await act(async () => { await vi.advanceTimersByTimeAsync(ms); });
}

async function renderPage(page: "home" | "detail", state: SseConnectionState) {
  await act(async () => {
    root.render(
      <QueryClientProvider client={client}>
        <SseConnectionContext.Provider value={state}>
          <MemoryRouter initialEntries={["/items/movie-1"]}>
            <Routes><Route path="/items/:itemId" element={page === "home"
              ? <HomePage user={{ id: "viewer", usernameNormalized: "viewer" }} />
              : <MediaDetailPage />} /></Routes>
          </MemoryRouter>
        </SseConnectionContext.Provider>
      </QueryClientProvider>,
    );
  });
  await advance(10);
}

it.each(["home", "detail"] as const)("stops %s polling online, polls every 30s offline, and stops again on reconnect", async (page) => {
  const requests = page === "home"
    ? [api.homeCarousel, api.homeLibraries, api.homeContinueWatching, api.homeLibrariesLatest]
    : [api.item, api.itemImages, api.playback];
  await renderPage(page, "connected");
  for (const request of requests) expect(request).toHaveBeenCalledTimes(1);
  await advance(60_000);
  for (const request of requests) expect(request).toHaveBeenCalledTimes(1);

  await renderPage(page, "disconnected");
  await advance(30_000);
  for (const request of requests) expect(request).toHaveBeenCalledTimes(2);

  await renderPage(page, "connected");
  await advance(60_000);
  for (const request of requests) expect(request).toHaveBeenCalledTimes(2);
});
