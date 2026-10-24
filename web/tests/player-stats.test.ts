import { describe, expect, it } from "vitest";
import type { MediaSource, WebPlaybackSessionStats } from "../src/lib/api/types";
import {
  buildPlayerInfoGroups,
  createClientStatsSampler,
  formatBitrate,
  formatBytes,
  formatCodec,
  formatContainer,
  formatFrameRate,
  playerInfoCopyText,
  summarizeSource,
  tierLabel,
} from "../src/features/player/player-stats";

const SOURCE: MediaSource = {
  id: "source-1",
  container: "matroska",
  bitrate: 15_000_000,
  streams: [
    {
      index: 0,
      type: "VIDEO",
      codec: "hevc",
      details: { Width: 3840, Height: 2160, RealFrameRate: "24000/1001", BitRate: 14_000_000 },
    },
    {
      index: 1,
      type: "AUDIO",
      codec: "eac3",
      language: "eng",
      details: { BitRate: 768_000, Channels: 6 },
    },
  ],
};

const SERVER_HLS: WebPlaybackSessionStats = {
  sessionId: "session-1",
  playSessionId: "lux-web:session-1",
  sourceId: "source-1",
  plan: "SERVER_HLS",
  tier: 4,
  state: "ACTIVE",
  expiresAt: 1_700_000_000,
  output: {
    container: "mp4",
    videoCodec: "h264",
    audioCodec: "aac",
    videoBitrate: null,
    audioBitrate: 192_000,
  },
  progress: {
    generation: 0,
    frame: 240,
    fps: 24,
    bitrateKbps: 2048.5,
    totalSize: 1_048_576,
    outTimeSeconds: 10,
    speed: 1.53,
    duplicatedFrames: 0,
    droppedFrames: 3,
    finished: false,
    updatedAt: 1_000,
  },
};

function rows(groups: ReturnType<typeof buildPlayerInfoGroups>, title: string) {
  const group = groups.find((entry) => entry.title === title);
  if (!group) throw new Error(`missing group ${title}`);
  return Object.fromEntries(group.rows.map((row) => [row.label, row.value]));
}

describe("player statistics formatting", () => {
  it("formats bitrates, bytes, codecs and containers for the panel", () => {
    expect(formatBitrate(768_000)).toBe("768 kbps");
    expect(formatBitrate(15_000_000)).toBe("15 Mbps");
    expect(formatBitrate(2_500_000)).toBe("2.5 Mbps");
    expect(formatBitrate(null)).toBeNull();
    expect(formatBytes(1_048_576)).toBe("1.0 MB");
    expect(formatBytes(512)).toBe("512 B");
    expect(formatCodec("h264")).toBe("H.264");
    expect(formatCodec("eac3")).toBe("E-AC-3");
    expect(formatCodec("av1")).toBe("AV1");
    expect(formatContainer("matroska")).toBe("MKV");
    expect(formatContainer("mov,mp4")).toBe("MOV·MP4");
    expect(formatFrameRate(23.976)).toBe("24.0 fps");
    expect(formatFrameRate(0)).toBeNull();
    expect(tierLabel(3)).toBe("档位 3 · 硬件转码");
    expect(tierLabel(null)).toBe("—");
  });

  it("summarizes a media source from ffprobe stream details", () => {
    expect(summarizeSource(SOURCE)).toEqual({
      container: "matroska",
      bitrate: 15_000_000,
      video: {
        codec: "hevc",
        width: 3840,
        height: 2160,
        frameRate: 24000 / 1001,
        bitrate: 14_000_000,
        title: null,
      },
      audio: {
        codec: "eac3",
        bitrate: 768_000,
        channels: 6,
        language: "eng",
        title: null,
      },
    });
    expect(summarizeSource(undefined)).toBeNull();
  });
});

describe("player information groups", () => {
  it("describes what a server-side transcode turns the source into", () => {
    const groups = buildPlayerInfoGroups({
      plan: "SERVER_HLS",
      tier: 4,
      source: summarizeSource(SOURCE),
      server: SERVER_HLS,
      client: {
        engineKind: "native",
        resolution: "1920×1080",
        frameRate: 23.98,
        droppedFrames: 1,
        totalFrames: 480,
        bufferSeconds: 12.5,
        bandwidthBitsPerSecond: 20_000_000,
        levelBitrate: 2_100_000,
        levelResolution: "1920×1080",
        clientSpeedX: null,
      },
      playback: { positionSeconds: 65, durationSeconds: 5_400, rate: 1, volume: 0.8, muted: false, paused: false },
      nowMs: 3_000,
    });

    expect(groups.map((group) => group.title)).toEqual(["播放", "来源", "输出", "实时"]);
    const source = rows(groups, "来源");
    expect(source["容器"]).toBe("MKV");
    expect(source["视频"]).toBe("HEVC · 3840×2160 · 24.0 fps");
    expect(source["视频码率"]).toBe("14 Mbps");
    expect(source["音频"]).toBe("E-AC-3 · 5.1 · ENG");
    expect(source["总码率"]).toBe("15 Mbps");

    const output = rows(groups, "输出");
    expect(output["输出容器"]).toBe("MP4");
    expect(output["视频"]).toBe("H.264 · 源 HEVC");
    expect(output["音频"]).toBe("AAC · 源 E-AC-3");
    expect(output["音频码率"]).toBe("192 kbps");
    expect(output["转码档位"]).toBe("档位 4 · 软件转码");

    const live = rows(groups, "实时");
    expect(live["编码帧率"]).toBe("24.0 fps");
    expect(live["编码速度"]).toBe("1.53×");
    expect(live["输出码率"]).toBe("2.0 Mbps");
    expect(live["已输出"]).toBe("1.0 MB");
    expect(live["丢帧 / 重复"]).toBe("3 / 0");
    expect(live["编码位置"]).toBe("00:10");
    expect(live["编码状态"]).toBe("编码中");
    expect(live["数据更新"]).toBe("2.0 秒前");
    expect(live["客户端帧率"]).toBe("24.0 fps");
    expect(live["解码帧 / 丢帧"]).toBe("480 / 1");
    expect(live["缓冲"]).toBe("12.5 秒");
    expect(live["带宽估算"]).toBe("20 Mbps");

    const playback = rows(groups, "播放");
    expect(playback["播放方式"]).toBe("服务端 HLS");
    expect(playback["服务端档位"]).toBe("档位 4 · 软件转码");
    expect(playback["播放进度"]).toBe("01:05 · 1:30:00");
    expect(playback["播放会话"]).toBe("lux-web:session-1（ACTIVE）");
  });

  it("describes direct play as an untouched stream", () => {
    const groups = buildPlayerInfoGroups({
      plan: "DIRECT",
      tier: 0,
      source: summarizeSource(SOURCE),
      server: null,
      client: null,
      playback: { positionSeconds: 0, durationSeconds: 60, rate: 1.5, volume: 0, muted: true, paused: true },
      nowMs: 0,
    });

    const output = rows(groups, "输出");
    expect(output["视频"]).toBe("直放，原始码流");
    expect(output["转码"]).toBe("未转码（服务端只读取字节）");

    const live = rows(groups, "实时");
    expect(live["推流方式"]).toBe("Range 直读（服务端不处理）");
    expect(live["推流码率"]).toBe("15 Mbps");
    expect(live["服务端编码"]).toBe("未启用（直放）");
    expect(live["编码帧率"]).toBeUndefined();

    const playback = rows(groups, "播放");
    expect(playback["播放方式"]).toBe("直连播放");
    expect(playback["音量"]).toBe("静音");
    expect(playback["状态"]).toBe("已暂停");
    expect(playback["播放速率"]).toBe("1.5×");
  });

  it("reports a pending or failed statistics request instead of stale numbers", () => {
    const pending = buildPlayerInfoGroups({
      plan: "SERVER_HLS",
      tier: 1,
      source: null,
      server: null,
      serverPending: true,
      client: null,
      playback: { positionSeconds: 0, durationSeconds: 0, rate: 1, volume: 1, muted: false, paused: true },
    });
    const live = rows(pending, "实时");
    expect(live["编码状态"]).toBe("等待 FFmpeg 首个进度报告");
    expect(rows(pending, "输出")["统计数据"]).toBe("正在读取服务端统计");

    const failed = buildPlayerInfoGroups({
      plan: "SERVER_HLS",
      tier: 1,
      source: null,
      server: null,
      serverError: "请求失败",
      client: null,
      playback: { positionSeconds: 0, durationSeconds: 0, rate: 1, volume: 1, muted: false, paused: true },
    });
    expect(rows(failed, "输出")["统计数据"]).toBe("请求失败");
    expect(rows(failed, "实时")["统计请求"]).toBe("请求失败");
  });

  it("builds a copyable session dump", () => {
    const groups = buildPlayerInfoGroups({
      plan: "SERVER_HLS",
      tier: 4,
      source: summarizeSource(SOURCE),
      server: SERVER_HLS,
      client: null,
      playback: { positionSeconds: 10, durationSeconds: 60, rate: 1, volume: 1, muted: false, paused: false },
      nowMs: 1_000,
    });
    const text = playerInfoCopyText(groups);
    expect(text).toContain("【播放】");
    expect(text).toContain("播放方式：服务端 HLS");
    expect(text).toContain("【输出】");
    expect(text).toContain("视频：H.264 · 源 HEVC");
  });
});

describe("client statistics sampler", () => {
  it("derives frame rate from playback quality deltas and reads buffer and engine metrics", () => {
    let clock = 1_000;
    const sampler = createClientStatsSampler(() => clock);
    const video = {
      videoWidth: 1920,
      videoHeight: 1080,
      currentTime: 10,
      buffered: { length: 1, start: () => 0, end: () => 30 },
      getVideoPlaybackQuality: () => ({ totalVideoFrames: 100, droppedVideoFrames: 2 }),
    } as unknown as HTMLVideoElement;
    const engine = {
      kind: "native",
      performance: null,
      stats: () => ({ bandwidthBitsPerSecond: 12_000_000, levelBitrate: 2_400_000, levelLabel: "1920×1080" }),
    };

    expect(sampler({ engine, video })).toMatchObject({
      engineKind: "native",
      resolution: "1920×1080",
      frameRate: null,
      droppedFrames: 2,
      totalFrames: 100,
      bufferSeconds: 20,
      bandwidthBitsPerSecond: 12_000_000,
      levelBitrate: 2_400_000,
      levelResolution: "1920×1080",
    });

    clock = 2_000;
    video.getVideoPlaybackQuality = () => ({ totalVideoFrames: 124, droppedVideoFrames: 4 });
    expect(sampler({ engine, video })).toMatchObject({ frameRate: 24, droppedFrames: 4 });
    expect(sampler({ engine: null, video: null })).toBeNull();
  });
});
