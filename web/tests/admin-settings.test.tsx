// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter } from "react-router-dom";
import { AdminSettingsPage } from "../src/features/admin/AdminSettingsPage";
import { api } from "../src/lib/api/client";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

const settings = {
  resumePlayedPercent: 90,
  resumeMinTicks: 1_200_000_000,
  mediaStrategy: {
    metadataLanguage: "zh-CN",
    imageLanguage: "zh-CN",
    region: "CN",
    scraperId: null,
    applyScope: "NEW_CONTENT",
    images: {
      poster: true,
      artwork: false,
      banner: false,
      logo: true,
      thumbnail: true,
      disc: false,
      wallpaper: false,
      maxBackdropCount: 1,
      minDownloadWidth: 1280,
    },
    subtitles: {
      autoDownload: false,
      languages: ["zh-CN"],
      forcedOnly: false,
      hearingImpaired: false,
    },
  },
  networkProxy: {
    configured: true,
    url: "http://192.168.1.2:7890/",
    hasCredentials: false,
    source: "settings",
    restartRequired: true,
  },
};

describe("AdminSettingsPage network proxy", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    vi.spyOn(api, "adminSettings").mockResolvedValue(settings);
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("shows the network proxy setting and saves its URL", async () => {
    const update = vi.spyOn(api, "updateAdminSettings").mockResolvedValue({
      ...settings,
      networkProxy: {
        ...settings.networkProxy,
        configured: true,
        url: "http://192.168.1.2:7890/",
        source: "settings",
      },
    });
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <AdminSettingsPage />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(container.textContent).toContain("网络代理设置");
    const input = container.querySelector<HTMLInputElement>("input[aria-label='网络代理地址']");
    expect(input).toBeTruthy();
    await act(async () => {
      [...container.querySelectorAll<HTMLButtonElement>("button")]
        .find((button) => button.textContent?.includes("保存网络代理"))
        ?.click();
    });

    expect(update).toHaveBeenCalledWith({ networkProxyUrl: "http://192.168.1.2:7890/" });
  });
});
