// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { HomePage } from "../src/features/home/HomePage";
import { api } from "../src/lib/api/client";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("HomePage shelves", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
    vi.restoreAllMocks();
  });

  it("shows accessible media libraries without a generic recently-added shelf", async () => {
    vi.spyOn(api, "home").mockResolvedValue({
      libraries: [{
        id: "library-1",
        name: "华语电影",
        kind: "MOVIE",
        coverImageUrl: "/covers/chinese.jpg",
      }],
      recommended: [],
      continueWatching: [],
      recentlyAdded: [{ id: "recent-1", title: "最近电影", itemType: "MOVIE" }],
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <HomePage />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(container.querySelector('[aria-label="我的媒体库"] .lux-library-card')?.textContent)
      .toContain("华语电影");
    expect(container.querySelector('[aria-label="最近添加"]')).toBeNull();
  });
});
