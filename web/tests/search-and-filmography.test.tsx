// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { PersonDetailPage } from "../src/features/detail/PersonDetailPage";
import { SearchPage } from "../src/features/search/SearchPage";
import { api } from "../src/lib/api/client";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

async function flushReact() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

describe("actor search and filmography", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;

  beforeEach(() => {
    container = document.createElement("div");
    document.body.append(container);
  });

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
    vi.restoreAllMocks();
  });

  it("shows actor results alongside media results and links to the person page", async () => {
    vi.spyOn(api, "search").mockResolvedValue({
      items: [{ id: "movie-1", title: "演员甲电影", itemType: "MOVIE", rating: 7.5, ratingSource: "TMDb" }],
      total: 1,
      page: 1,
      pageSize: 24,
    });
    vi.spyOn(api, "searchPeople").mockResolvedValue({
      items: [{ id: "42", name: "演员甲", imageUrl: "/api/v1/people/tmdb/42/image" }],
      total: 1,
      page: 1,
      pageSize: 12,
    });
    root = createRoot(container!);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/search?q=演员甲"]}>
            <Routes><Route path="search" element={<SearchPage />} /></Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await flushReact();
    await flushReact();

    const personLink = container?.querySelector<HTMLAnchorElement>(".lux-person-search-card");
    expect(personLink?.textContent).toContain("演员甲");
    expect(personLink?.getAttribute("href")).toBe("/people/42");
    expect(container?.textContent).toContain("演员甲电影");
    const mediaResults = container?.querySelector("section[aria-labelledby='media-search-heading']");
    expect(mediaResults?.querySelector(".lux-rating.is-compact")).not.toBeNull();
    expect(mediaResults?.querySelector(".lux-rating-source")).toBeNull();
    expect(mediaResults?.querySelector(".lux-rating svg")).toBeNull();
  });

  it("loads and displays the actor's accessible movie and series works", async () => {
    vi.spyOn(api, "person").mockResolvedValue({ id: "42", name: "演员甲" });
    vi.spyOn(api, "personItems").mockResolvedValue({
      items: [
        { id: "movie-1", title: "演员甲电影", itemType: "MOVIE", rating: 7.5, ratingSource: "TMDb" },
        { id: "series-1", title: "演员甲剧集", itemType: "SERIES", rating: 8.2, ratingSource: "TMDb" },
      ],
      total: 2,
      page: 1,
      pageSize: 24,
    });
    root = createRoot(container!);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/people/42"]}>
            <Routes><Route path="people/:personId" element={<PersonDetailPage />} /></Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await flushReact();
    await flushReact();

    expect(container?.querySelector(".lux-person-works")?.textContent).toContain("参演作品");
    expect(container?.textContent).toContain("演员甲电影");
    expect(container?.textContent).toContain("演员甲剧集");
    expect(container?.querySelectorAll(".lux-person-works .lux-rating.is-compact")).toHaveLength(2);
    expect(container?.querySelectorAll(".lux-person-works .lux-rating-source")).toHaveLength(0);
    expect(container?.querySelectorAll(".lux-person-works .lux-rating svg")).toHaveLength(0);
    expect(api.personItems).toHaveBeenCalledWith("42", 1);
  });

  it("loads the next page when the actor has more than one page of works", async () => {
    vi.spyOn(api, "person").mockResolvedValue({ id: "42", name: "演员甲" });
    const personItems = vi.spyOn(api, "personItems").mockImplementation(async (_personId, page = 1) => ({
      items: page === 1
        ? [{ id: "movie-1", title: "演员甲电影", itemType: "MOVIE" }]
        : [{ id: "series-2", title: "演员甲第二部剧集", itemType: "SERIES" }],
      total: 25,
      page,
      pageSize: 24,
    }));
    root = createRoot(container!);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/people/42"]}>
            <Routes><Route path="people/:personId" element={<PersonDetailPage />} /></Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await flushReact();
    await flushReact();

    const loadMore = container?.querySelector<HTMLButtonElement>(".lux-person-works-actions button");
    expect(loadMore?.textContent).toContain("加载更多");
    await act(async () => loadMore?.click());
    await flushReact();
    await flushReact();

    expect(container?.textContent).toContain("演员甲第二部剧集");
    expect(personItems).toHaveBeenNthCalledWith(2, "42", 2);
  });
});
