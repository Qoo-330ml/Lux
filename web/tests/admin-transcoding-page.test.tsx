// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AdminTranscodingPage } from "../src/features/admin/AdminTranscodingPage";
import { api } from "../src/lib/api/client";
import type {
  AdminSettings,
  AdminTranscodingSessions,
  AdminTranscodingSettings,
} from "../src/lib/api/types";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

const TRANSCODING: AdminTranscodingSettings = {
  enabled: true,
  hardwareEncoder: "h264_videotoolbox",
  remuxSessions: 4,
  hardwareSessions: 2,
  softwareSessions: 1,
  sessionQuotaMb: 2_048,
  globalBudgetMb: 8_192,
  minFreeSpaceMb: 512,
  maxVideoBitrateKbps: 8_000,
  softwarePreset: "fast",
  maxOutputHeight: 1_080,
  audioBitrateKbps: 192,
  hardwareDevice: "/dev/dri/renderD128",
  hardwareDecode: false,
};

const SETTINGS = {
  resumePlayedPercent: 90,
  resumeMinTicks: 1_200_000_000,
  loginBackgroundSource: "STATIC",
  mediaStrategy: {
    metadataLanguage: "zh-CN",
    imageLanguage: "zh-CN",
    region: "CN",
    scraperId: null,
    applyScope: "NEW_CONTENT",
    images: {},
    subtitles: {},
    showMetadataPending: true,
  },
  transcoding: TRANSCODING,
  transcodingStatus: {
    encoder: "h264_videotoolbox",
    available: true,
    checkedAt: Date.UTC(2026, 9, 25, 8, 53, 0),
    error: null,
  },
  transcodingOptions: {
    hardwareEncoders: ["h264_nvenc", "h264_qsv", "h264_vaapi", "h264_videotoolbox"],
    softwarePresets: ["ultrafast", "superfast", "veryfast", "faster", "fast", "medium"],
  },
} as unknown as AdminSettings;

const HISTORY = {
  page: 1,
  pageSize: 10,
  total: 2,
  items: [
    {
      id: "history-1",
      sessionId: "session-old",
      userId: "user-1",
      userName: "Admin",
      itemId: "item-1",
      title: "Transcode Demo",
      itemType: "MOVIE",
      mediaSourceId: "source-1",
      tier: 3,
      outcome: "COMPLETED",
      startedAt: 1_000,
      finishedAt: 1_060,
      wallSeconds: 60,
      encodedSeconds: 20,
      averageSpeed: 0.33,
      lastFps: 425.7,
      outputBytes: 1_048_576,
      failureReason: null,
    },
    {
      id: "history-2",
      sessionId: "session-failed",
      userId: "user-1",
      userName: "Admin",
      itemId: "item-2",
      title: "Broken GPU Test",
      itemType: "MOVIE",
      mediaSourceId: "source-2",
      tier: 3,
      outcome: "FAILED",
      startedAt: 2_000,
      finishedAt: 2_001,
      wallSeconds: 1,
      encodedSeconds: null,
      averageSpeed: null,
      lastFps: null,
      outputBytes: null,
      failureReason: "HLS_START_FAILED",
    },
  ],
};

const SESSIONS: AdminTranscodingSessions = {
  summary: {
    activeCount: 1,
    hardwareCount: 1,
    softwareCount: 0,
    streamCopyCount: 0,
    outputBytes: 1_048_576,
  },
  sessions: [
    {
      sessionId: "session-1",
      playSessionId: "lux-web:session-1",
      userId: "user-1",
      userName: "Admin",
      itemId: "item-1",
      title: "Transcode Demo",
      itemType: "MOVIE",
      tier: 3,
      state: "ACTIVE",
      container: "mp4",
      videoCodec: "h264",
      audioCodec: "aac",
      audioBitrate: 192_000,
      createdAt: 1_000,
      updatedAt: 2_000,
      expiresAt: 3_000,
      progress: {
        fps: 425.7,
        speed: 17.7,
        bitrateKbps: null,
        latestSegmentBitrateKbps: 8_203.7,
        averageBitrateKbps: 3_100,
        directoryBytes: 1_048_576,
        outTimeSeconds: 20,
        droppedFrames: 1,
        finished: false,
        updatedAt: 2_000,
      },
    },
  ],
};

describe("AdminTranscodingPage", () => {
  let container: HTMLDivElement;
  let root: Root;
  let queryClient: QueryClient;

  beforeEach(() => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    vi.spyOn(api, "adminSettings").mockResolvedValue(SETTINGS);
    vi.spyOn(api, "adminTranscodingSessions").mockResolvedValue(SESSIONS);
    vi.spyOn(api, "adminTranscodingHistory").mockResolvedValue(HISTORY);
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  async function render() {
    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <AdminTranscodingPage />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
  }

  it("groups transcoding settings and shows live session progress", async () => {
    await render();
    expect(container.textContent).toContain("转码");
    expect(container.textContent).toContain("转码设置");
    expect(container.textContent).toContain("实时转码进度");
    expect(container.textContent).toContain("自检通过");
    // Summary chips.
    expect(container.textContent).toContain("活动会话");
    expect(container.textContent).toContain("硬件编码");
    // Session row: title, user, tier, live metrics.
    const row = container.querySelector("[aria-label='转码会话']");
    expect(row).not.toBeNull();
    expect(row?.textContent).toContain("Transcode Demo");
    expect(row?.textContent).toContain("Admin");
    expect(row?.textContent).toContain("档位 3 · 硬件转码");
    expect(row?.textContent).toContain("425.7 fps");
    expect(row?.textContent).toContain("17.70×");
    expect(row?.textContent).toContain("8.2 Mbps");
    expect(row?.textContent).toContain("1.0 MB");
    expect(row?.textContent).toContain("00:20");
    expect(row?.textContent).toContain("编码中");
  });

  it("stops a live session and shows the history with outcomes", async () => {
    const stop = vi.spyOn(api, "stopAdminTranscodingSession").mockResolvedValue(undefined);
    const confirm = vi.fn().mockReturnValue(true);
    vi.stubGlobal("confirm", confirm);
    await render();

    const stopButton = container.querySelector<HTMLButtonElement>("button[aria-label='停止 Transcode Demo']");
    expect(stopButton).not.toBeNull();
    await act(async () => {
      stopButton?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(confirm).toHaveBeenCalledTimes(1);
    expect(stop).toHaveBeenCalledWith("session-1");

    const history = container.querySelector("[aria-label='转码历史记录']");
    expect(history).not.toBeNull();
    expect(history?.textContent).toContain("完成");
    expect(history?.textContent).toContain("失败");
    expect(history?.textContent).toContain("失败原因：HLS_START_FAILED");
    expect(history?.textContent).toContain("平均速度 0.33×");
    expect(container.textContent).toContain("共 2 条");
    vi.unstubAllGlobals();
  });

  it("does not stop when the confirmation is dismissed", async () => {
    const stop = vi.spyOn(api, "stopAdminTranscodingSession").mockResolvedValue(undefined);
    vi.stubGlobal("confirm", vi.fn().mockReturnValue(false));
    await render();
    const stopButton = container.querySelector<HTMLButtonElement>("button[aria-label='停止 Transcode Demo']");
    await act(async () => {
      stopButton?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(stop).not.toHaveBeenCalled();
    vi.unstubAllGlobals();
  });

  it("shows an empty state when nothing is transcoding", async () => {
    vi.spyOn(api, "adminTranscodingSessions").mockResolvedValue({
      summary: { activeCount: 0, hardwareCount: 0, softwareCount: 0, streamCopyCount: 0, outputBytes: 0 },
      sessions: [],
    });
    await render();
    expect(container.textContent).toContain("当前没有正在进行的转码");
  });
});
