// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AdminTranscodingPanel } from "../src/features/admin/AdminTranscodingPanel";
import { api } from "../src/lib/api/client";
import type { AdminSettings, AdminTranscodingSettings } from "../src/lib/api/types";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

const TRANSCODING: AdminTranscodingSettings = {
  enabled: true,
  hardwareEncoder: "NONE",
  remuxSessions: 4,
  hardwareSessions: 2,
  softwareSessions: 1,
  sessionQuotaMb: 2_048,
  globalBudgetMb: 8_192,
  minFreeSpaceMb: 512,
  maxVideoBitrateKbps: 0,
  softwarePreset: "veryfast",
  maxOutputHeight: 0,
  audioBitrateKbps: 192,
  hardwareDevice: "/dev/dri/renderD128",
  hardwareDecode: false,
};

const SETTINGS: AdminSettings = {
  resumePlayedPercent: 90,
  resumeMinTicks: 1_200_000_000,
  loginBackgroundSource: "STATIC",
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
      thumbnailScrapingMode: "SCRAPER_FIRST",
      disc: false,
      wallpaper: false,
      writeToMetadata: false,
      maxBackdropCount: 1,
      minDownloadWidth: 1280,
    },
    subtitles: {
      autoDownload: false,
      languages: ["zh-CN"],
      forcedOnly: false,
      hearingImpaired: false,
    },
    showMetadataPending: true,
  },
  transcoding: TRANSCODING,
  transcodingStatus: {
    encoder: "NONE",
    available: false,
    checkedAt: 0,
    error: null,
  },
  transcodingOptions: {
    hardwareEncoders: ["h264_nvenc", "h264_qsv", "h264_vaapi", "h264_videotoolbox"],
    softwarePresets: ["ultrafast", "superfast", "veryfast", "faster", "fast", "medium"],
  },
};

describe("AdminTranscodingPanel", () => {
  let container: HTMLDivElement;
  let root: Root;
  let queryClient: QueryClient;

  beforeEach(() => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    queryClient = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  async function render(settings: AdminSettings) {
    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <AdminTranscodingPanel settings={settings} />
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
  }

  function input(label: string) {
    return container.querySelector<HTMLInputElement>(`input[aria-label='${label}']`);
  }

  function select(label: string) {
    return container.querySelector<HTMLSelectElement>(`select[aria-label='${label}']`);
  }

  async function type(label: string, value: string) {
    const element = input(label);
    if (!element) throw new Error(`missing input ${label}`);
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
    await act(async () => {
      setter?.call(element, value);
      element.dispatchEvent(new Event("input", { bubbles: true }));
    });
  }

  it("shows the stored policy and the encoder allowlist", async () => {
    await render(SETTINGS);
    expect(container.textContent).toContain("转码设置");
    expect(container.textContent).toContain("部署层 LUX_HLS_HW_ENCODER 优先于此处选择");
    expect(input("启用服务端转码")?.checked).toBe(true);
    expect(select("硬件编码器")?.value).toBe("NONE");
    expect([...container.querySelectorAll<HTMLOptionElement>("select[aria-label='硬件编码器'] option")].map((option) => option.value))
      .toEqual(["NONE", "h264_nvenc", "h264_qsv", "h264_vaapi", "h264_videotoolbox"]);
    expect(select("软件编码 preset")?.value).toBe("veryfast");
    expect(select("最大输出分辨率")?.value).toBe("0");
    expect(input("封装转换并发上限")?.value).toBe("4");
    expect(input("音频码率")?.value).toBe("192");
    expect(input("硬件设备路径")?.value).toBe("/dev/dri/renderD128");
    expect(input("启用硬件解码")?.checked).toBe(false);
    expect(container.textContent).toContain("未启用硬件编码");
  });

  it("saves the edited policy as one payload", async () => {
    const update = vi.spyOn(api, "updateAdminSettings").mockResolvedValue({
      ...SETTINGS,
      transcoding: {
        ...TRANSCODING,
        enabled: false,
        hardwareEncoder: "h264_videotoolbox",
        softwareSessions: 0,
        softwarePreset: "fast",
        maxOutputHeight: 1080,
        maxVideoBitrateKbps: 8_000,
        audioBitrateKbps: 256,
      },
    });
    await render(SETTINGS);

    await act(async () => {
      input("启用服务端转码")?.click();
    });
    await act(async () => {
      const encoder = select("硬件编码器");
      if (encoder) {
        encoder.value = "h264_videotoolbox";
        encoder.dispatchEvent(new Event("change", { bubbles: true }));
      }
      const preset = select("软件编码 preset");
      if (preset) {
        preset.value = "fast";
        preset.dispatchEvent(new Event("change", { bubbles: true }));
      }
      const height = select("最大输出分辨率");
      if (height) {
        height.value = "1080";
        height.dispatchEvent(new Event("change", { bubbles: true }));
      }
    });
    await type("软件转码并发上限", "0");
    await type("视频码率上限", "8000");
    await type("音频码率", "256");
    await type("硬件设备路径", "/dev/dri/renderD129");
    await act(async () => {
      input("启用硬件解码")?.click();
    });

    await act(async () => {
      [...container.querySelectorAll<HTMLButtonElement>("button")]
        .find((button) => button.textContent?.includes("保存转码设置"))
        ?.click();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(update).toHaveBeenCalledTimes(1);
    expect(update).toHaveBeenCalledWith({
      transcoding: {
        enabled: false,
        hardwareEncoder: "h264_videotoolbox",
        remuxSessions: 4,
        hardwareSessions: 2,
        softwareSessions: 0,
        sessionQuotaMb: 2_048,
        globalBudgetMb: 8_192,
        minFreeSpaceMb: 512,
        maxVideoBitrateKbps: 8_000,
        softwarePreset: "fast",
        maxOutputHeight: 1_080,
        audioBitrateKbps: 256,
        hardwareDevice: "/dev/dri/renderD129",
        hardwareDecode: true,
      },
    });
    expect(container.textContent).toContain("设置已保存，将在新的播放会话生效");
  });

  it("keeps the draft visible when the server rejects a value", async () => {
    vi.spyOn(api, "updateAdminSettings").mockRejectedValue(new Error("服务端转码设置无效"));
    await render(SETTINGS);
    await act(async () => {
      const preset = select("软件编码 preset");
      if (preset) {
        preset.value = "medium";
        preset.dispatchEvent(new Event("change", { bubbles: true }));
      }
    });
    await act(async () => {
      [...container.querySelectorAll<HTMLButtonElement>("button")]
        .find((button) => button.textContent?.includes("保存转码设置"))
        ?.click();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(container.textContent).toContain("服务端转码设置无效");
    expect(select("软件编码 preset")?.value).toBe("medium");
  });

  it("shows the self-test result and can re-run it", async () => {
    const probe = vi.spyOn(api, "probeAdminTranscoding").mockResolvedValue({
      transcodingStatus: {
        encoder: "h264_vaapi",
        available: true,
        checkedAt: Date.UTC(2026, 9, 25, 1, 0, 0),
        error: null,
      },
    });
    await render({
      ...SETTINGS,
      transcoding: { ...TRANSCODING, hardwareEncoder: "h264_vaapi" },
      transcodingStatus: {
        encoder: "h264_vaapi",
        available: false,
        checkedAt: Date.UTC(2026, 9, 25, 0, 0, 0),
        error: "Failed to initialise VAAPI connection: -1 (unknown libva error).",
      },
    });
    expect(container.textContent).toContain("自检失败");
    expect(container.textContent).toContain("Failed to initialise VAAPI");

    await act(async () => {
      [...container.querySelectorAll<HTMLButtonElement>("button")]
        .find((button) => button.textContent?.includes("重新检测"))
        ?.click();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(probe).toHaveBeenCalledTimes(1);
    expect(container.textContent).toContain("自检通过");
  });

  it("disables the probe button when no hardware encoder is selected", async () => {
    await render(SETTINGS);
    const button = [...container.querySelectorAll<HTMLButtonElement>("button")]
      .find((entry) => entry.textContent?.includes("重新检测"));
    expect(button?.disabled).toBe(true);
  });

  it("explains an older server that returns no transcoding block", async () => {
    await render({ ...SETTINGS, transcoding: undefined, transcodingOptions: undefined });
    expect(container.textContent).toContain("当前服务器版本未返回转码设置");
  });
});
