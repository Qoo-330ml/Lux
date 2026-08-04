// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { LibraryCard } from "../src/features/home/media";
import { api } from "../src/lib/api/client";

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

  it("opens a custom context menu for whole-library operations", async () => {
    const reidentify = vi.spyOn(api, "startLibraryMetadataReidentify").mockResolvedValue({
      totalCount: 125,
      jobCount: 2,
      jobs: [
        { id: "job-1", status: "QUEUED", totalCount: 100 },
        { id: "job-2", status: "QUEUED", totalCount: 25 },
      ],
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
    expect(document.body.querySelector("[role=menu]")?.textContent).toContain("识别");
    expect(document.body.querySelector("[role=menu]")?.textContent).toContain("扫描媒体库文件");

    await act(async () => {
      document.body.querySelector<HTMLButtonElement>("[data-library-action=reidentify]")?.click();
    });
    expect(reidentify).toHaveBeenCalledWith("library-1");
    expect(container.querySelector("[role=status]")?.textContent).toContain("125");

    await act(async () => {
      container.querySelector<HTMLElement>(".lux-library-card")?.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 100, clientY: 120 }));
    });
    await act(async () => {
      document.body.querySelector<HTMLButtonElement>("[data-library-action=scan]")?.click();
    });
    expect(scan).toHaveBeenCalledWith("library-1");
    expect(container.querySelector("[role=status]")?.textContent).toContain("扫描");
  });
});
