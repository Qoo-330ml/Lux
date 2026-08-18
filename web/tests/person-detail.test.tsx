// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { MediaCast } from "../src/features/detail/MediaCast";
import { PersonDetailPage } from "../src/features/detail/PersonDetailPage";
import { api } from "../src/lib/api/client";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("person detail", () => {
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

  it("makes every actor card link to the person page", () => {
    root = createRoot(container!);
    act(() => {
      root?.render(
        <MemoryRouter>
          <MediaCast actors={[{ id: "person-1", name: "演员甲", character: "角色甲" }]} />
        </MemoryRouter>,
      );
    });

    expect(container?.querySelector<HTMLAnchorElement>(".lux-media-cast-card a")?.getAttribute("href"))
      .toBe("/people/person-1");
    expect(container?.querySelector(".lux-media-cast-card a")?.textContent).toContain("演员甲");
  });

  it("loads and displays person photo, biography and role", async () => {
    vi.spyOn(api, "person").mockResolvedValue({
      id: "person-1",
      name: "演员甲",
      character: "角色甲",
      imageUrl: "/api/v1/people/tmdb/person-1/image",
      biography: "演员甲的生平介绍",
      birthday: "1970-01-01",
      knownForDepartment: "Acting",
      placeOfBirth: "测试城市",
    });
    root = createRoot(container!);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/people/person-1"]}>
            <Routes>
              <Route path="people/:personId" element={<PersonDetailPage />} />
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(container?.querySelector("h1")?.textContent).toBe("演员甲");
    expect(container?.textContent).toContain("角色甲");
    expect(container?.textContent).toContain("演员甲的生平介绍");
    expect(container?.querySelector<HTMLImageElement>(".lux-person-detail-photo")?.src)
      .toContain("/api/v1/people/tmdb/person-1/image");
    expect(container?.textContent).toContain("1970-01-01");
    expect(container?.textContent).toContain("测试城市");
  });
});
