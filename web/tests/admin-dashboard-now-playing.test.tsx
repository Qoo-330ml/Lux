// @vitest-environment jsdom
import { createRoot, type Root } from "react-dom/client";
import { act } from "react";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { AdminDashboardNowPlaying } from "../src/features/admin/AdminDashboardNowPlaying";
import type { AdminPlaybackSession } from "../src/lib/api/types";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

const session: AdminPlaybackSession = {
  id: "session-1",
  userId: "user-1",
  userName: "Admin",
  itemId: "item-1",
  title: "电影",
  itemType: "MOVIE",
  posterAvailable: false,
  positionTicks: 1,
  durationTicks: 2,
  state: "PLAYING",
  isPaused: false,
  lastEventAt: 1,
  deviceId: "device-1",
  remoteIp: "8.8.8.8",
  remoteIpLocation: {
    location: "美国 · 加利福尼亚州 · 山景城",
    district: "Santa Clara",
    street: "Amphitheatre Parkway",
    isp: "Google",
  },
  playSessionId: "play-session-1",
};

describe("AdminDashboardNowPlaying", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
  });

  it("shows the cached IP location and ISP details", () => {
    act(() => {
      root.render(
        <MemoryRouter>
          <AdminDashboardNowPlaying sessions={[session]} />
        </MemoryRouter>,
      );
    });

    const location = container.querySelector('[aria-label="IP 归属地"]');
    expect(location?.textContent).toContain("美国 · 加利福尼亚州 · 山景城");
    expect(location?.textContent).toContain("Santa Clara · Amphitheatre Parkway · Google");
  });

  it("shows server transcoding and output codecs", () => {
    const transcodingSession: AdminPlaybackSession = {
      ...session,
      playMethod: "Transcode",
      serverTier: 4,
      output: {
        container: "ts",
        videoCodec: "h264",
        audioCodec: "aac",
        videoBitrate: 1_000_000,
        audioBitrate: 192_000,
      },
      source: {
        id: "source-1",
        video: { codec: "hevc", title: "4K HDR" },
        audio: { codec: "ac3", language: "zh-CN", title: "立体声" },
      },
    };

    act(() => {
      root.render(
        <MemoryRouter>
          <AdminDashboardNowPlaying sessions={[transcodingSession]} />
        </MemoryRouter>,
      );
    });

    expect(container.textContent).toContain("播放：转码播放 · 服务端 HLS · 软件转码 · TS");
    expect(container.textContent).toContain("视频：H.264 · 原始 HEVC · 4K HDR · 1.0 Mbps");
    expect(container.textContent).toContain("音频：AAC · zh-CN · 原始 AC3 · 立体声 · 192 kbps");
  });
});
