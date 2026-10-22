// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { LibraryCard } from "../src/features/home/media";
import { api } from "../src/lib/api/client";
import { GLOBAL_COLLECTION_LIBRARY_ID } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("LibraryCard", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("uses the configured cover image when available", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter>
          <LibraryCard
            library={{
              id: "library-1",
              name: "电影",
              kind: "MOVIE",
              coverImageUrl: "/api/v1/libraries/library-1/cover",
            }}
          />
        </MemoryRouter>,
      );
    });

    expect(container.querySelector<HTMLImageElement>(".lux-library-cover")?.src).toContain(
      "/api/v1/libraries/library-1/cover",
    );
    expect(container.querySelector(".lux-library-card-cover")).not.toBeNull();
    expect(container.querySelector(".lux-library-cover-full")).not.toBeNull();
    expect(container.querySelector(".lux-library-icon")).toBeNull();
  });

  it("prefetches the library page on pointer hover and keyboard focus", async () => {
    const onPrefetch = vi.fn();
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter>
          <LibraryCard library={{ id: "library-1", name: "电影", kind: "MOVIE" }} onPrefetch={onPrefetch} />
        </MemoryRouter>,
      );
    });

    const link = container.querySelector<HTMLAnchorElement>(".lux-library-card-copy-link");
    await act(async () => {
      link?.dispatchEvent(new MouseEvent("mouseover", { bubbles: true }));
      link?.dispatchEvent(new FocusEvent("focusin", { bubbles: true }));
    });

    expect(onPrefetch).toHaveBeenCalledTimes(2);
  });

  it("shows the virtual collection entry without physical-library operations", async () => {
    const onPrefetch = vi.fn();
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter>
          <LibraryCard
            library={{ id: GLOBAL_COLLECTION_LIBRARY_ID, name: "合集", kind: "MIXED" }}
            onPrefetch={onPrefetch}
          />
        </MemoryRouter>,
      );
    });

    const link = container.querySelector<HTMLAnchorElement>(".lux-library-card-copy-link");
    expect(link?.getAttribute("href")).toBe(`/libraries/${GLOBAL_COLLECTION_LIBRARY_ID}`);
    expect(container.querySelector(".lux-library-card-menu-trigger")).toBeNull();
    const contextMenuEvent = new MouseEvent("contextmenu", { bubbles: true, cancelable: true });
    await act(async () => {
      container.querySelector<HTMLElement>(".lux-library-card")?.dispatchEvent(contextMenuEvent);
      link?.dispatchEvent(new FocusEvent("focusin", { bubbles: true }));
    });
    expect(contextMenuEvent.defaultPrevented).toBe(false);
    expect(document.body.querySelector("[role=menu]")).toBeNull();
    expect(onPrefetch).not.toHaveBeenCalled();
  });

  it("opens a custom context menu for whole-library operations", async () => {
    const reidentify = vi.spyOn(api, "startLibraryMetadataReidentify").mockResolvedValue({
      totalCount: 125,
      job: { id: "job-1", status: "QUEUED", mode: "FILL_MISSING", totalCount: 125, processedCount: 0, createdAt: 0 },
    });
    const scan = vi.spyOn(api, "startAdminScan").mockResolvedValue({
      job: { id: "scan-job", libraryId: "library-1", jobType: "SCAN", status: "QUEUED" },
    });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter>
          <LibraryCard library={{ id: "library-1", name: "电影", kind: "MOVIE" }} />
        </MemoryRouter>,
      );
    });

    const contextMenuEvent = new MouseEvent("contextmenu", {
      bubbles: true,
      cancelable: true,
      clientX: 100,
      clientY: 120,
    });
    await act(async () => {
      container.querySelector<HTMLElement>(".lux-library-card")?.dispatchEvent(contextMenuEvent);
    });

    expect(contextMenuEvent.defaultPrevented).toBe(true);
    expect(document.body.querySelector("[role=menu]")?.textContent).toContain("元数据匹配");
    expect(document.body.querySelector("[role=menu]")?.textContent).toContain("扫描媒体库文件");

    await act(async () => {
      document.body.querySelector<HTMLButtonElement>("[data-library-action=reidentify]")?.click();
    });
    expect(reidentify).toHaveBeenCalledWith("library-1");
    expect(container.querySelector("[role=status]")?.textContent).toContain("元数据匹配");

    await act(async () => {
      container.querySelector<HTMLElement>(".lux-library-card")?.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 100, clientY: 120 }));
    });
    await act(async () => {
      document.body.querySelector<HTMLButtonElement>("[data-library-action=scan]")?.click();
    });
    expect(scan).toHaveBeenCalledWith("library-1");
    expect(container.querySelector("[role=status]")?.textContent).toContain("扫描");
  });

  it("keeps its portaled menu open for inside pointer events and closes outside", async () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter>
          <LibraryCard library={{ id: "library-1", name: "电影", kind: "MOVIE" }} />
        </MemoryRouter>,
      );
    });
    await act(async () => {
      container.querySelector<HTMLElement>(".lux-library-card")?.dispatchEvent(new MouseEvent("contextmenu", {
        bubbles: true,
        cancelable: true,
        clientX: 100,
        clientY: 120,
      }));
    });

    const menu = document.body.querySelector<HTMLElement>("[role=menu]");
    const outside = document.createElement("button");
    document.body.append(outside);
    await act(async () => menu?.querySelector<HTMLElement>("[data-library-action=scan]")
      ?.dispatchEvent(new Event("pointerdown", { bubbles: true })));
    expect(document.body.querySelector("[role=menu]")).not.toBeNull();

    await act(async () => outside.dispatchEvent(new Event("pointerdown", { bubbles: true })));

    expect(document.body.querySelector("[role=menu]")).toBeNull();
    outside.remove();
  });
});
