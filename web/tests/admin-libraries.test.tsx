// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter } from "react-router-dom";
import { AdminLibrariesPage } from "../src/features/admin/AdminLibrariesPage";
import { api } from "../src/lib/api/client";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const library = {
  id: "library-1",
  name: "01每日更新",
  kind: "MIXED",
  coverImageUrl: "/covers/daily-updates.jpg",
  itemCount: 12,
  isEnabled: true,
  realtimeWatchEnabled: true,
  roots: [{
    id: "root-1",
    libraryId: "library-1",
    canonicalPath: "/media/strm/video/每日更新",
    displayPath: "/media/strm/video/每日更新",
    isAvailable: true,
    isWritable: true,
  }],
};

describe("AdminLibrariesPage library cards", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [library] });
    vi.spyOn(api, "adminPlugins").mockResolvedValue({ plugins: [] });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  async function renderPage() {
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <AdminLibrariesPage />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
  }

  it("renders a library card with its cover, type, and root path", async () => {
    await renderPage();

    expect(container.querySelector(".lux-admin-library-grid")).toBeTruthy();
    expect(container.querySelector(".lux-admin-library-cover")?.getAttribute("src")).toBe("/covers/daily-updates.jpg");
    expect(container.textContent).toContain("01每日更新");
    expect(container.textContent).toContain("混合内容");
    expect(container.textContent).toContain("/media/strm/video/每日更新");
  });

  it("opens the library actions menu from the card overflow button", async () => {
    await renderPage();

    const menuButton = container.querySelector<HTMLButtonElement>("[aria-label='打开 01每日更新 操作菜单']");
    expect(menuButton).toBeTruthy();

    await act(async () => menuButton?.click());

    expect(container.querySelector('[role="menu"]')?.textContent).toContain("编辑");
    expect(container.querySelector('[role="menu"]')?.textContent).toContain("扫描媒体库文件");
  });

  it("opens the edit dialog from the library actions menu", async () => {
    await renderPage();

    await act(async () => {
      container.querySelector<HTMLButtonElement>("[aria-label='打开 01每日更新 操作菜单']")?.click();
    });
    const editAction = [...container.querySelectorAll<HTMLButtonElement>('[role="menu"] button')]
      .find((button) => button.textContent?.includes("编辑"));
    expect(editAction).toBeTruthy();

    await act(async () => editAction?.click());

    expect(container.querySelector('[role="dialog"]')?.textContent).toContain("01每日更新");
    expect(container.querySelector<HTMLInputElement>('[aria-label="01每日更新 媒体库名称"]')?.value).toBe("01每日更新");
  });
});
