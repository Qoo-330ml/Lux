// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, expect, it, vi } from "vitest";
import { LibraryPage } from "../src/features/library/LibraryPage";
import { api } from "../src/lib/api/client";

const rendered = vi.hoisted(() => [] as string[]);
vi.mock("../src/features/home/media-card-overlay", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../src/features/home/media-card-overlay")>();
  return { MediaCardOverlay: (props: Parameters<typeof actual.MediaCardOverlay>[0]) => {
    rendered.push(props.item.id);
    return <actual.MediaCardOverlay {...props} />;
  } };
});

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: Root | undefined;
let container: HTMLDivElement;
let client: QueryClient;
afterEach(() => { act(() => root?.unmount()); client?.clear(); container?.remove(); vi.restoreAllMocks(); });

it("selects and deselects items without rendering other loaded library cards", async () => {
  vi.spyOn(api, "libraries").mockResolvedValue({ libraries: [{ id: "library", name: "电影", kind: "MOVIE" }] });
  vi.spyOn(api, "libraryItems").mockResolvedValue({ items: [1, 2, 3].map((id) => ({ id: `movie-${id}`, title: `电影 ${id}`, itemType: "MOVIE" })), page: 1, pageSize: 24, total: 3 });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  await act(async () => root?.render(<QueryClientProvider client={client}><MemoryRouter initialEntries={["/libraries/library"]}><Routes><Route path="/libraries/:libraryId" element={<LibraryPage />} /></Routes></MemoryRouter></QueryClientProvider>));
  await act(async () => { await vi.waitFor(() => expect(container.querySelectorAll(".lux-media-card")).toHaveLength(3)); });
  await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="开启媒体库多选"]')?.click());
  for (const [id, count] of [[1, 1], [2, 2], [1, 1]]) {
    rendered.length = 0;
    await act(async () => container.querySelector<HTMLInputElement>(`input[aria-label="选择 电影 ${id}"]`)?.click());
    expect(container.querySelectorAll(".is-selected")).toHaveLength(count);
    expect(container.textContent).toContain(`已选 ${count} 项`);
    expect(rendered).toEqual([`movie-${id}`]);
  }
});
