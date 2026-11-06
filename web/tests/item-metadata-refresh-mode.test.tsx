// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MediaDetailActions, MediaDetailActionControls } from "../src/features/detail/MediaDetailActions";
import { MediaSourceProvider } from "../src/features/detail/MediaSourcePanel";
import { MediaCard } from "../src/features/home/media";
import { useItemMetadataRefreshMode } from "../src/features/media/useItemMetadataRefreshMode";
import { api } from "../src/lib/api/client";
import { queryKeys } from "../src/lib/api/query-keys";
import type { AdminSettings, MediaItem, MetadataRefreshMode } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const adminSession = {
  user: { id: "admin-1", usernameNormalized: "admin", canManageServer: true },
};

function settings(globalMode?: MetadataRefreshMode): AdminSettings {
  return {
    resumePlayedPercent: 90,
    resumeMinTicks: 1_200_000_000,
    mediaStrategy: { metadataRefreshMode: globalMode },
  } as AdminSettings;
}

function adminLibrary(mode?: MetadataRefreshMode) {
  return {
    id: "library-1",
    name: "电影库",
    kind: "MOVIE",
    roots: [],
    mediaStrategy: mode ? { metadataRefreshMode: mode } : null,
  };
}

function item(overrides: Partial<MediaItem> = {}): MediaItem {
  return {
    id: "item-1",
    title: "示例电影",
    itemType: "MOVIE",
    libraryId: "library-1",
    ...overrides,
  };
}

let container: HTMLDivElement | undefined;
let root: Root | undefined;
let client: QueryClient | undefined;

async function mount(ui: ReactNode, canManageServer: boolean) {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  client.setQueryData(queryKeys.me, {
    user: { id: "user-1", usernameNormalized: "user", canManageServer },
  });
  await act(async () => {
    root?.render(<QueryClientProvider client={client!}>{ui}</QueryClientProvider>);
  });
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

function refreshAction() {
  return [...document.body.querySelectorAll<HTMLButtonElement>("[role='menu'] button")]
    .find((button) => button.dataset.action === "refresh-metadata");
}

async function openFirstMenu() {
  await act(async () => {
    container?.querySelectorAll<HTMLButtonElement>(".lux-media-actions-trigger")[0]?.click();
  });
}

async function clickRefresh() {
  await act(async () => {
    refreshAction()?.click();
  });
}

afterEach(() => {
  if (root) act(() => root?.unmount());
  client?.clear();
  container?.remove();
  document.body.replaceChildren();
  vi.restoreAllMocks();
});

function ModeProbe({ mediaItem }: { mediaItem: MediaItem }) {
  const mode = useItemMetadataRefreshMode(mediaItem, true);
  return <span data-mode>{mode ?? "unresolved"}</span>;
}

describe("useItemMetadataRefreshMode", () => {
  it.each([
    {
      name: "library full override",
      libraryMode: "FULL_REFRESH" as const,
      globalMode: "FILL_MISSING" as const,
      libraryId: "library-1",
      expected: "FULL_REFRESH",
    },
    {
      name: "global full without a library override",
      libraryMode: undefined,
      globalMode: "FULL_REFRESH" as const,
      libraryId: "library-1",
      expected: "FULL_REFRESH",
    },
    {
      name: "library fill override beats a global full",
      libraryMode: "FILL_MISSING" as const,
      globalMode: "FULL_REFRESH" as const,
      libraryId: "library-1",
      expected: "FILL_MISSING",
    },
    {
      name: "fill missing when neither level sets a mode",
      libraryMode: undefined,
      globalMode: undefined,
      libraryId: "library-1",
      expected: "FILL_MISSING",
    },
    {
      name: "global mode when the item has no library",
      libraryMode: undefined,
      globalMode: "FULL_REFRESH" as const,
      libraryId: null,
      expected: "FULL_REFRESH",
    },
  ])("resolves $name", async ({ libraryMode, globalMode, libraryId, expected }) => {
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [adminLibrary(libraryMode)] } as never);
    vi.spyOn(api, "adminSettings").mockResolvedValue(settings(globalMode));

    await mount(<ModeProbe mediaItem={item({ libraryId })} />, true);

    await vi.waitFor(() => {
      expect(container?.querySelector("[data-mode]")?.textContent).toBe(expected);
    });
  });

  it("stays unresolved when the admin queries are still pending", async () => {
    vi.spyOn(api, "adminLibraries").mockReturnValue(new Promise(() => undefined));
    vi.spyOn(api, "adminSettings").mockReturnValue(new Promise(() => undefined));

    await mount(<ModeProbe mediaItem={item()} />, true);

    expect(container?.querySelector("[data-mode]")?.textContent).toBe("unresolved");
  });

  it("stays unresolved when an admin query fails", async () => {
    vi.spyOn(api, "adminLibraries").mockRejectedValue(new Error("forbidden"));
    vi.spyOn(api, "adminSettings").mockResolvedValue(settings("FULL_REFRESH"));

    await mount(<ModeProbe mediaItem={item()} />, true);

    await vi.waitFor(() => expect(api.adminLibraries).toHaveBeenCalledTimes(1));
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(container?.querySelector("[data-mode]")?.textContent).toBe("unresolved");
  });

  it("uses the global mode for a library-less item without reading the library list", async () => {
    const libraries = vi.spyOn(api, "adminLibraries").mockRejectedValue(new Error("forbidden"));
    vi.spyOn(api, "adminSettings").mockResolvedValue(settings("FULL_REFRESH"));

    await mount(<ModeProbe mediaItem={item({ libraryId: null })} />, true);

    await vi.waitFor(() => {
      expect(container?.querySelector("[data-mode]")?.textContent).toBe("FULL_REFRESH");
    });
    expect(libraries).not.toHaveBeenCalled();
  });

  it("never queries the admin endpoints for a non-admin viewer", async () => {
    const libraries = vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [] });
    const settingsQuery = vi.spyOn(api, "adminSettings").mockResolvedValue(settings("FULL_REFRESH"));

    await mount(<ModeProbe mediaItem={item()} />, false);

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(libraries).not.toHaveBeenCalled();
    expect(settingsQuery).not.toHaveBeenCalled();
  });
});

describe("item metadata refresh menu", () => {
  it("labels and runs a library full refresh from the home card", async () => {
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [adminLibrary("FULL_REFRESH")] } as never);
    vi.spyOn(api, "adminSettings").mockResolvedValue(settings("FILL_MISSING"));
    const refresh = vi.spyOn(api, "startItemMetadataRefresh").mockResolvedValue({} as never);

    await mount(
      <MemoryRouter>
        <MediaCard item={item()} />
      </MemoryRouter>,
      true,
    );
    await openFirstMenu();

    await vi.waitFor(() => expect(refreshAction()?.textContent?.trim()).toBe("全量刷新元数据"));
    expect(refreshAction()?.getAttribute("title")).toBe("重新获取并覆盖未锁定的元数据和图片");

    await clickRefresh();
    expect(refresh).toHaveBeenCalledWith("item-1", "FULL_REFRESH");
    expect(container?.querySelector('[role="status"]')?.textContent).toBe("刷新任务已提交");
  });

  it("labels and runs a fill-missing refresh for a non-admin without admin queries", async () => {
    const libraries = vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [adminLibrary("FULL_REFRESH")] } as never);
    const settingsQuery = vi.spyOn(api, "adminSettings").mockResolvedValue(settings("FULL_REFRESH"));
    const refresh = vi.spyOn(api, "startItemMetadataRefresh").mockResolvedValue({} as never);

    await mount(
      <MemoryRouter>
        <MediaCard item={item()} />
      </MemoryRouter>,
      false,
    );
    await openFirstMenu();

    expect(refreshAction()?.textContent?.trim()).toBe("刷新元数据");
    expect(refreshAction()?.getAttribute("title")).toBeNull();
    await clickRefresh();

    expect(refresh).toHaveBeenCalledWith("item-1", "FILL_MISSING");
    expect(libraries).not.toHaveBeenCalled();
    expect(settingsQuery).not.toHaveBeenCalled();
  });

  it("keeps the neutral label and fills missing when the admin queries fail", async () => {
    vi.spyOn(api, "adminLibraries").mockRejectedValue(new Error("forbidden"));
    vi.spyOn(api, "adminSettings").mockRejectedValue(new Error("forbidden"));
    const refresh = vi.spyOn(api, "startItemMetadataRefresh").mockResolvedValue({} as never);

    await mount(
      <MemoryRouter>
        <MediaCard item={item()} />
      </MemoryRouter>,
      true,
    );
    await openFirstMenu();
    await vi.waitFor(() => expect(api.adminLibraries).toHaveBeenCalledTimes(1));
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(refreshAction()?.textContent?.trim()).toBe("刷新元数据");
    await clickRefresh();
    expect(refresh).toHaveBeenCalledWith("item-1", "FILL_MISSING");
  });

  it("keeps the neutral label and fills missing while the admin queries are pending", async () => {
    vi.spyOn(api, "adminLibraries").mockReturnValue(new Promise(() => undefined));
    vi.spyOn(api, "adminSettings").mockReturnValue(new Promise(() => undefined));
    const refresh = vi.spyOn(api, "startItemMetadataRefresh").mockResolvedValue({} as never);

    await mount(
      <MemoryRouter>
        <MediaCard item={item()} />
      </MemoryRouter>,
      true,
    );
    await openFirstMenu();

    expect(refreshAction()?.textContent?.trim()).toBe("刷新元数据");
    await clickRefresh();
    expect(refresh).toHaveBeenCalledWith("item-1", "FILL_MISSING");
  });

  it("lets a fill-missing library override a full-refresh global strategy", async () => {
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [adminLibrary("FILL_MISSING")] } as never);
    vi.spyOn(api, "adminSettings").mockResolvedValue(settings("FULL_REFRESH"));
    const refresh = vi.spyOn(api, "startItemMetadataRefresh").mockResolvedValue({} as never);

    await mount(
      <MemoryRouter>
        <MediaCard item={item()} />
      </MemoryRouter>,
      true,
    );
    await openFirstMenu();

    await vi.waitFor(() => expect(refreshAction()?.textContent?.trim()).toBe("补全缺失元数据"));
    expect(refreshAction()?.getAttribute("title")).toBe("只补全缺失的元数据和图片");
    await clickRefresh();
    expect(refresh).toHaveBeenCalledWith("item-1", "FILL_MISSING");
  });

  it("labels and runs the resolved mode from the detail action menu", async () => {
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [adminLibrary("FULL_REFRESH")] } as never);
    vi.spyOn(api, "adminSettings").mockResolvedValue(settings("FILL_MISSING"));
    vi.spyOn(api, "playback").mockResolvedValue({});
    const refresh = vi.spyOn(api, "startItemMetadataRefresh").mockResolvedValue({} as never);
    const detailItem = item({ mediaSources: [{ id: "source-1", isDefault: true }] });

    await mount(
      <MemoryRouter>
        <MediaSourceProvider item={detailItem}>
          <MediaDetailActions item={detailItem} pendingReview={false}>
            <MediaDetailActionControls />
          </MediaDetailActions>
        </MediaSourceProvider>
      </MemoryRouter>,
      true,
    );

    await openFirstMenu();
    await vi.waitFor(() => expect(refreshAction()?.textContent?.trim()).toBe("全量刷新元数据"));
    expect(refreshAction()?.getAttribute("title")).toBe("重新获取并覆盖未锁定的元数据和图片");

    await clickRefresh();
    expect(refresh).toHaveBeenCalledWith("item-1", "FULL_REFRESH");
    expect(container?.querySelector('[role="status"]')?.textContent)
      .toBe("元数据刷新任务已提交，可在管理任务中查看进度。");
  });

  it("deduplicates the admin queries across a page of cards", async () => {
    const libraries = vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [adminLibrary("FULL_REFRESH")] } as never);
    const settingsQuery = vi.spyOn(api, "adminSettings").mockResolvedValue(settings("FILL_MISSING"));

    await mount(
      <MemoryRouter>
        {[1, 2, 3, 4].map((id) => <MediaCard key={id} item={item({ id: `item-${id}`, title: `电影 ${id}` })} />)}
      </MemoryRouter>,
      true,
    );
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(libraries).toHaveBeenCalledTimes(1);
    expect(settingsQuery).toHaveBeenCalledTimes(1);
  });
});
