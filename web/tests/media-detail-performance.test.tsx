// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, expect, it, vi } from "vitest";
import { MediaDetailPage } from "../src/features/detail/MediaDetailPage";
import { MediaCast } from "../src/features/detail/MediaCast";
import { MediaStills } from "../src/features/detail/MediaStills";
import { MediaNfoPanel } from "../src/features/detail/MediaNfoPanel";
import { api } from "../src/lib/api/client";
import { queryKeys } from "../src/lib/api/query-keys";
import type { MediaItem } from "../src/lib/api/types";

vi.mock("../src/features/detail/MediaCast", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../src/features/detail/MediaCast")>();
  return { MediaCast: vi.fn((props) => createElement(actual.MediaCast, props)) };
});
vi.mock("../src/features/detail/MediaStills", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../src/features/detail/MediaStills")>();
  return { MediaStills: vi.fn((props) => createElement(actual.MediaStills, props)) };
});
vi.mock("../src/features/detail/MediaNfoPanel", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../src/features/detail/MediaNfoPanel")>();
  return { MediaNfoPanel: vi.fn((props) => createElement(actual.MediaNfoPanel, props)) };
});

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: Root | undefined;
let container: HTMLDivElement;
let client: QueryClient;

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
  client?.clear();
  vi.restoreAllMocks();
  vi.clearAllMocks();
});

async function renderDetail() {
  const item: MediaItem = {
    id: "movie-1", itemType: "MOVIE", title: "性能回归电影",
    actors: Array.from({ length: 100 }, (_, index) => ({ id: `actor-${index}`, name: `演员 ${index}` })),
    nfo: { studios: ["测试公司"], genres: ["剧情"] },
    mediaSources: [{ id: "source-1", container: "mkv", isDefault: true }],
  };
  vi.spyOn(api, "item").mockResolvedValue(item);
  vi.spyOn(api, "playback").mockResolvedValue({ isFavorite: false });
  vi.spyOn(api, "itemImages").mockResolvedValue({ images: [{
    id: "still-1", itemId: item.id, imageType: "FANART", imageIndex: 0, url: "/still.png",
  }] });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  await act(async () => {
    root?.render(
      <QueryClientProvider client={client}>
        <MemoryRouter initialEntries={["/items/movie-1"]}>
          <Routes><Route path="items/:itemId" element={<MediaDetailPage />} /></Routes>
        </MemoryRouter>
      </QueryClientProvider>,
    );
  });
  await flushQueries();
}

async function flushQueries() {
  await act(async () => { await new Promise((resolve) => setTimeout(resolve, 20)); });
}

function renderCounts() {
  return [MediaCast, MediaStills, MediaNfoPanel].map((component) => vi.mocked(component).mock.calls.length);
}

it("updates playback controls without rendering cast, stills, or NFO again", async () => {
  await renderDetail();
  expect(container.querySelectorAll(".lux-media-cast-card")).toHaveLength(100);
  const before = renderCounts();
  for (let index = 0; index < 5; index++) {
    client.setQueryData(queryKeys.playback("movie-1"), { isFavorite: index % 2 === 0 });
    await flushQueries();
  }
  expect(container.querySelector("[data-action='toggle-favorite']")?.getAttribute("aria-pressed")).toBe("true");
  const extraRenders = renderCounts().map((count, index) => count - before[index]);
  console.info("Five playback updates: extra cast/stills/NFO renders", extraRenders);
  expect(extraRenders).toEqual([0, 0, 0]);
});
