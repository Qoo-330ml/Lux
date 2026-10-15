// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { HomePage } from "../src/features/home/HomePage";
import { api } from "../src/lib/api/client";
import { queryKeys } from "../src/lib/api/query-keys";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let container: HTMLDivElement;
let root: Root;
let client: QueryClient;
const user = { id: "query-policy-user", usernameNormalized: "viewer" };
const libraries = [{ id: "library-1", name: "电影库", kind: "MOVIE" }];

beforeEach(() => {
  localStorage.clear();
  sessionStorage.clear();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  vi.spyOn(api, "homeCarousel").mockResolvedValue({ recommended: [] });
  vi.spyOn(api, "homeLibraries").mockResolvedValue({ libraries });
  vi.spyOn(api, "homeContinueWatching").mockResolvedValue({ items: [], total: 0 });
  vi.spyOn(api, "homeLibrariesLatest").mockResolvedValue({
    libraries: [{ libraryId: "library-1", items: [] }],
  });
});

afterEach(() => {
  act(() => root.unmount());
  client.clear();
  container.remove();
  vi.restoreAllMocks();
});

async function renderHome(visible = true) {
  await act(async () => {
    root.render(
      <QueryClientProvider client={client}>
        <MemoryRouter>{visible ? <HomePage user={user} /> : null}</MemoryRouter>
      </QueryClientProvider>,
    );
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

it("reuses all four fresh home sections on a return visit", async () => {
  await renderHome();
  await act(async () => { await vi.waitFor(() => expect(api.homeLibrariesLatest).toHaveBeenCalledTimes(1)); });
  await renderHome(false);
  await renderHome();

  expect(api.homeCarousel).toHaveBeenCalledTimes(1);
  expect(api.homeLibraries).toHaveBeenCalledTimes(1);
  expect(api.homeContinueWatching).toHaveBeenCalledTimes(1);
  expect(api.homeLibrariesLatest).toHaveBeenCalledTimes(1);
});

it("starts latest from cached library IDs while a stale library refresh is pending", async () => {
  client.setQueryData(queryKeys.libraries, { libraries }, { updatedAt: Date.now() - 60_000 });
  let finishLibraries: ((response: { libraries: typeof libraries }) => void) | undefined;
  vi.mocked(api.homeLibraries).mockImplementation(() => new Promise((resolve) => { finishLibraries = resolve; }));

  await renderHome();
  expect(api.homeLibraries).toHaveBeenCalledTimes(1);
  expect(api.homeLibrariesLatest).toHaveBeenCalledWith(["library-1"], expect.any(AbortSignal));

  await act(async () => {
    finishLibraries?.({ libraries: [{ id: "library-2", name: "剧集库", kind: "SERIES" }] });
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
  await vi.waitFor(() => expect(api.homeLibrariesLatest).toHaveBeenCalledWith(["library-2"], expect.any(AbortSignal)));
  expect(container.querySelector('[aria-label="最新电影库"]')).toBeNull();
});

it("refreshes home sections after the 30 second freshness window expires", async () => {
  await renderHome();
  await act(async () => { await vi.waitFor(() => expect(api.homeLibrariesLatest).toHaveBeenCalledTimes(1)); });
  await renderHome(false);
  for (const query of client.getQueryCache().getAll()) {
    client.setQueryData(query.queryKey, query.state.data, { updatedAt: Date.now() - 31_000 });
  }
  await renderHome();

  expect(api.homeCarousel).toHaveBeenCalledTimes(2);
  expect(api.homeLibraries).toHaveBeenCalledTimes(2);
  expect(api.homeContinueWatching).toHaveBeenCalledTimes(2);
  expect(api.homeLibrariesLatest).toHaveBeenCalledTimes(2);
});
