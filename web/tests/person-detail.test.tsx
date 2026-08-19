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
      providerIds: { Imdb: "nm1234567", Tmdb: "123456" },
      genres: ["Drama"],
      tags: ["MDC"],
      productionLocations: ["日本"],
      premiereDate: "2000-01-02",
      productionYear: 2000,
      taglines: ["MDC 标语"],
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
    expect(container?.textContent).toContain("Drama");
    expect(container?.textContent).toContain("MDC");
    expect(container?.textContent).toContain("日本");
    expect(container?.textContent).toContain("2000-01-02");
    expect(container?.textContent).toContain("MDC 标语");
    expect(container?.textContent).toContain("nm1234567");
  });

  it("renders escaped line breaks in a biography as actual line breaks", async () => {
    vi.spyOn(api, "person").mockResolvedValue({
      id: "person-2",
      name: "演员乙",
      biography: "第一行<br>第二行<br/>第三行",
    });
    root = createRoot(container!);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/people/person-2"]}>
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

    const biography = container?.querySelector(".lux-person-overview p");
    expect(biography?.querySelectorAll("br")).toHaveLength(2);
    expect(biography?.textContent).toBe("第一行第二行第三行");
    expect(biography?.textContent).not.toContain("<br>");
  });

  it("lets a server manager edit and save person metadata", async () => {
    vi.spyOn(api, "person").mockResolvedValue({ id: "person-3", name: "演员丙", biography: "旧简介" });
    const updatePerson = vi.spyOn(api, "updatePerson").mockResolvedValue({
      id: "person-3",
      name: "演员丙（编辑）",
      biography: "新简介",
    });
    root = createRoot(container!);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/people/person-3"]}>
            <Routes>
              <Route path="people/:personId" element={<PersonDetailPage user={{ id: "admin", canManageServer: true }} />} />
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    await act(async () => {
      container?.querySelector<HTMLButtonElement>("[aria-label='编辑人物资料']")?.click();
    });
    const nameInput = container?.querySelector<HTMLInputElement>("#person-name");
    expect(nameInput).not.toBeNull();
    await act(async () => {
      if (nameInput) {
        const valueSetter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
        valueSetter?.call(nameInput, "演员丙（编辑）");
        nameInput.dispatchEvent(new Event("input", { bubbles: true }));
        nameInput.dispatchEvent(new Event("change", { bubbles: true }));
      }
      container?.querySelector<HTMLButtonElement>("[aria-label='保存人物资料']")?.click();
    });
    expect(updatePerson).toHaveBeenCalledWith("person-3", expect.objectContaining({ name: "演员丙（编辑）" }));
  });
});
