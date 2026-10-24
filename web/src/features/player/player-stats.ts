import type { MediaSource, MediaStream, WebPlaybackSessionStats } from "../../lib/api/types";
import type { PlaybackEngineStats } from "./playback-engine";

/**
 * Pure formatting and model building for the player's information panel.
 *
 * The panel mirrors what Emby/Jellyfin and YouTube expose from "stats for nerds": what the server
 * planned, what it is producing right now, and what the browser is actually decoding. Everything
 * here is display-only and must never be used to drive playback decisions.
 */

const MISSING = "—";

export type PlayerInfoRow = {
  label: string;
  value: string;
};

export type PlayerInfoGroup = {
  title: string;
  rows: PlayerInfoRow[];
};

export type PlayerClientStats = {
  engineKind: string;
  resolution: string | null;
  frameRate: number | null;
  droppedFrames: number | null;
  totalFrames: number | null;
  bufferSeconds: number | null;
  bandwidthBitsPerSecond: number | null;
  levelBitrate: number | null;
  levelResolution: string | null;
  clientSpeedX: number | null;
};

export type PlayerInfoSource = {
  container: string | null;
  bitrate: number | null;
  video: {
    codec: string | null;
    width: number | null;
    height: number | null;
    frameRate: number | null;
    bitrate: number | null;
    title: string | null;
  } | null;
  audio: {
    codec: string | null;
    bitrate: number | null;
    channels: number | null;
    language: string | null;
    title: string | null;
  } | null;
};

export type PlayerInfoInput = {
  plan: string | null | undefined;
  tier: number | null | undefined;
  source: PlayerInfoSource | null;
  server: WebPlaybackSessionStats | null | undefined;
  serverPending?: boolean;
  serverError?: string | null;
  client: PlayerClientStats | null;
  playback: {
    positionSeconds: number;
    durationSeconds: number;
    rate: number;
    volume: number;
    muted: boolean;
    paused: boolean;
  };
  /** Millisecond clock used for the "数据更新" row; injected so tests stay deterministic. */
  nowMs?: number;
};

export function formatBitrate(bitsPerSecond: number | null | undefined): string | null {
  if (typeof bitsPerSecond !== "number" || !Number.isFinite(bitsPerSecond) || bitsPerSecond <= 0) return null;
  if (bitsPerSecond < 1_000_000) return `${Math.round(bitsPerSecond / 1_000)} kbps`;
  const megabits = bitsPerSecond / 1_000_000;
  return `${megabits >= 10 ? Math.round(megabits) : megabits.toFixed(1)} Mbps`;
}

export function formatKbps(kilobitsPerSecond: number | null | undefined): string | null {
  if (typeof kilobitsPerSecond !== "number" || !Number.isFinite(kilobitsPerSecond) || kilobitsPerSecond <= 0) return null;
  return formatBitrate(kilobitsPerSecond * 1_000);
}

export function formatBytes(bytes: number | null | undefined): string | null {
  if (typeof bytes !== "number" || !Number.isFinite(bytes) || bytes <= 0) return null;
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value >= 10 || unit === 0 ? Math.round(value) : value.toFixed(1)} ${units[unit]}`;
}

export function formatFrameRate(frameRate: number | null | undefined): string | null {
  if (typeof frameRate !== "number" || !Number.isFinite(frameRate) || frameRate <= 0) return null;
  return `${frameRate.toFixed(frameRate >= 10 ? 1 : 2)} fps`;
}

export function formatSpeed(speed: number | null | undefined): string | null {
  if (typeof speed !== "number" || !Number.isFinite(speed) || speed <= 0) return null;
  return `${speed.toFixed(2)}×`;
}

export function formatResolution(
  width: number | null | undefined,
  height: number | null | undefined,
): string | null {
  if (typeof width !== "number" || typeof height !== "number") return null;
  if (!Number.isFinite(width) || !Number.isFinite(height) || width <= 0 || height <= 0) return null;
  return `${Math.round(width)}×${Math.round(height)}`;
}

const CODEC_LABELS: Record<string, string> = {
  h264: "H.264",
  avc: "H.264",
  avc1: "H.264",
  hevc: "HEVC",
  h265: "HEVC",
  hvc1: "HEVC",
  av1: "AV1",
  vp9: "VP9",
  vp8: "VP8",
  mpeg4: "MPEG-4",
  mpeg2video: "MPEG-2",
  aac: "AAC",
  ac3: "AC-3",
  eac3: "E-AC-3",
  dts: "DTS",
  truehd: "TrueHD",
  flac: "FLAC",
  opus: "Opus",
  mp3: "MP3",
  vorbis: "Vorbis",
};

export function formatCodec(codec: string | null | undefined): string | null {
  if (!codec) return null;
  const normalized = codec.trim().toLowerCase();
  if (!normalized) return null;
  return CODEC_LABELS[normalized] ?? codec.trim().toUpperCase();
}

const CONTAINER_LABELS: Record<string, string> = {
  mkv: "MKV",
  matroska: "MKV",
  webm: "WebM",
  mp4: "MP4",
  mov: "MOV",
  m4v: "M4V",
  ts: "TS",
  mpegts: "TS",
  avi: "AVI",
  flv: "FLV",
  wmv: "WMV",
};

export function formatContainer(container: string | null | undefined): string | null {
  if (!container) return null;
  const parts = container
    .split(",")
    .map((part) => part.trim().toLowerCase())
    .filter(Boolean)
    .map((part) => CONTAINER_LABELS[part] ?? part.toUpperCase());
  return parts.length > 0 ? parts.join("·") : null;
}

export function formatChannels(channels: number | null | undefined): string | null {
  if (typeof channels !== "number" || !Number.isFinite(channels) || channels <= 0) return null;
  if (channels === 1) return "单声道";
  if (channels === 2) return "立体声";
  if (channels === 6) return "5.1";
  if (channels === 8) return "7.1";
  return `${Math.round(channels)} 声道`;
}

export function tierLabel(tier: number | null | undefined): string {
  switch (tier) {
    case 0:
      return "档位 0 · 直放";
    case 1:
      return "档位 1 · 封装转换";
    case 2:
      return "档位 2 · 音频转码";
    case 3:
      return "档位 3 · 硬件转码";
    case 4:
      return "档位 4 · 软件转码";
    default:
      return typeof tier === "number" ? `档位 ${tier}` : MISSING;
  }
}

export function planLabel(plan: string | null | undefined): string {
  switch (plan) {
    case "DIRECT":
      return "直连播放";
    case "SERVER_HLS":
      return "服务端 HLS";
    case "UNSUPPORTED":
      return "不支持";
    default:
      return plan ? plan : MISSING;
  }
}

export function engineLabel(engineKind: string | null | undefined): string {
  switch (engineKind) {
    case "native":
      return "浏览器原生解码";
    case "client-mkv":
      return "浏览器 MKV 解码";
    case "client-hevc":
      return "浏览器 HEVC 解码";
    default:
      return engineKind ? engineKind : MISSING;
  }
}

export function formatDuration(seconds: number | null | undefined): string | null {
  if (typeof seconds !== "number" || !Number.isFinite(seconds) || seconds < 0) return null;
  const total = Math.floor(seconds);
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const remainder = total % 60;
  const pad = (value: number) => String(value).padStart(2, "0");
  return hours > 0 ? `${hours}:${pad(minutes)}:${pad(remainder)}` : `${pad(minutes)}:${pad(remainder)}`;
}

export function formatAge(updatedAtMs: number | null | undefined, nowMs: number): string | null {
  if (typeof updatedAtMs !== "number" || !Number.isFinite(updatedAtMs) || updatedAtMs <= 0) return null;
  const seconds = Math.max(0, (nowMs - updatedAtMs) / 1000);
  if (seconds < 1) return "刚刚";
  if (seconds < 60) return `${seconds.toFixed(seconds < 10 ? 1 : 0)} 秒前`;
  return `${Math.round(seconds / 60)} 分钟前`;
}

function detailNumber(stream: MediaStream | undefined, keys: readonly string[]): number | null {
  if (!stream?.details) return null;
  for (const key of keys) {
    const value = stream.details[key];
    const parsed = numericValue(value);
    if (parsed !== null) return parsed;
  }
  return null;
}

function numericValue(value: unknown): number | null {
  if (typeof value === "number") return Number.isFinite(value) ? value : null;
  if (typeof value !== "string") return null;
  const trimmed = value.trim();
  if (!trimmed || trimmed.toUpperCase() === "N/A") return null;
  const fraction = /^(\d+(?:\.\d+)?)\/(\d+(?:\.\d+)?)$/.exec(trimmed);
  if (fraction) {
    const numerator = Number(fraction[1]);
    const denominator = Number(fraction[2]);
    return denominator > 0 ? numerator / denominator : null;
  }
  const parsed = Number(trimmed);
  return Number.isFinite(parsed) ? parsed : null;
}

function streamOfType(source: MediaSource | undefined, type: "VIDEO" | "AUDIO"): MediaStream | undefined {
  return (source?.streams ?? []).find((stream) => (stream.type ?? "").toUpperCase() === type);
}

export function summarizeSource(source: MediaSource | undefined): PlayerInfoSource | null {
  if (!source) return null;
  const video = streamOfType(source, "VIDEO");
  const audio = streamOfType(source, "AUDIO");
  return {
    container: source.container ?? null,
    bitrate: typeof source.bitrate === "number" ? source.bitrate : null,
    video: video
      ? {
        codec: video.codec ?? null,
        width: detailNumber(video, ["Width"]),
        height: detailNumber(video, ["Height"]),
        frameRate: detailNumber(video, ["RealFrameRate", "AverageFrameRate"]),
        bitrate: detailNumber(video, ["BitRate"]),
        title: video.title ?? null,
      }
      : null,
    audio: audio
      ? {
        codec: audio.codec ?? null,
        bitrate: detailNumber(audio, ["BitRate"]),
        channels: detailNumber(audio, ["Channels"]),
        language: audio.language ?? null,
        title: audio.title ?? null,
      }
      : null,
  };
}

export function bufferedAheadSeconds(
  video: Pick<HTMLVideoElement, "buffered" | "currentTime"> | null | undefined,
): number | null {
  if (!video || !video.buffered || video.buffered.length === 0) return null;
  const ranges = video.buffered;
  for (let index = 0; index < ranges.length; index += 1) {
    const start = ranges.start(index);
    const end = ranges.end(index);
    if (video.currentTime >= start - 0.25 && video.currentTime <= end) {
      return Math.max(0, end - video.currentTime);
    }
  }
  return Math.max(0, ranges.end(ranges.length - 1) - video.currentTime);
}

export type ClientStatsSamplerInput = {
  engine:
    | {
      kind: string;
      performance: { speedX: number } | null;
      stats?: () => PlaybackEngineStats | null;
    }
    | null;
  video: HTMLVideoElement | null;
};

/**
 * Samples browser-side playback metrics once per call.
 *
 * Frame rate is derived from `getVideoPlaybackQuality()` deltas, so the sampler keeps one previous
 * reading. Callers reset it by creating a new sampler, which happens whenever the panel reopens.
 */
export function createClientStatsSampler(
  now: () => number = () => (typeof performance !== "undefined" ? performance.now() : Date.now()),
) {
  let previous: { atMs: number; frames: number } | null = null;
  return function sample(input: ClientStatsSamplerInput): PlayerClientStats | null {
    const { engine, video } = input;
    if (!video) {
      previous = null;
      return null;
    }
    const quality = typeof video.getVideoPlaybackQuality === "function"
      ? video.getVideoPlaybackQuality()
      : null;
    const atMs = now();
    let frameRate: number | null = null;
    if (quality) {
      if (previous && atMs - previous.atMs >= 250 && quality.totalVideoFrames >= previous.frames) {
        frameRate = (quality.totalVideoFrames - previous.frames) / ((atMs - previous.atMs) / 1000);
      }
      previous = { atMs, frames: quality.totalVideoFrames };
    }
    const engineStats = engine?.stats?.() ?? null;
    return {
      engineKind: engine?.kind ?? "native",
      resolution: formatResolution(video.videoWidth, video.videoHeight),
      frameRate,
      droppedFrames: quality ? quality.droppedVideoFrames : null,
      totalFrames: quality ? quality.totalVideoFrames : null,
      bufferSeconds: bufferedAheadSeconds(video),
      bandwidthBitsPerSecond: engineStats?.bandwidthBitsPerSecond ?? null,
      levelBitrate: engineStats?.levelBitrate ?? null,
      levelResolution: engineStats?.levelLabel ?? null,
      clientSpeedX: engine?.performance?.speedX ?? null,
    };
  };
}

function value(value: string | null | undefined): string {
  return value && value.trim() ? value : MISSING;
}

function push(rows: PlayerInfoRow[], label: string, rowValue: string | null | undefined) {
  rows.push({ label, value: value(rowValue) });
}

function joinParts(parts: Array<string | null | undefined>): string | null {
  const values = parts.filter((part): part is string => Boolean(part && part.trim()));
  return values.length > 0 ? values.join(" · ") : null;
}

export function buildPlayerInfoGroups(input: PlayerInfoInput): PlayerInfoGroup[] {
  const { plan, tier, source, server, client, playback } = input;
  const nowMs = input.nowMs ?? Date.now();
  const isServerHls = plan === "SERVER_HLS";
  const output = server?.output;
  const progress = server?.progress ?? null;

  const playbackRows: PlayerInfoRow[] = [];
  push(playbackRows, "播放方式", planLabel(plan));
  push(playbackRows, "服务端档位", tierLabel(server?.tier ?? tier));
  push(playbackRows, "解码引擎", engineLabel(client?.engineKind));
  push(playbackRows, "播放进度", joinParts([
    formatDuration(playback.positionSeconds),
    formatDuration(playback.durationSeconds),
  ]));
  push(playbackRows, "播放速率", `${playback.rate.toFixed(2).replace(/0+$/, "").replace(/\.$/, "")}×`);
  push(
    playbackRows,
    "音量",
    playback.muted ? "静音" : `${Math.round(Math.max(0, Math.min(1, playback.volume)) * 100)}%`,
  );
  push(playbackRows, "状态", playback.paused ? "已暂停" : "播放中");
  push(playbackRows, "播放会话", server?.playSessionId ? `${server.playSessionId}（${server.state}）` : null);

  const sourceRows: PlayerInfoRow[] = [];
  push(sourceRows, "容器", formatContainer(source?.container));
  push(
    sourceRows,
    "视频",
    joinParts([
      formatCodec(source?.video?.codec),
      formatResolution(source?.video?.width, source?.video?.height),
      formatFrameRate(source?.video?.frameRate),
    ]),
  );
  push(sourceRows, "视频码率", formatBitrate(source?.video?.bitrate));
  push(
    sourceRows,
    "音频",
    joinParts([
      formatCodec(source?.audio?.codec),
      formatChannels(source?.audio?.channels),
      source?.audio?.language ? source.audio.language.toUpperCase() : null,
    ]),
  );
  push(sourceRows, "音频码率", formatBitrate(source?.audio?.bitrate));
  push(sourceRows, "总码率", formatBitrate(source?.bitrate));

  const outputRows: PlayerInfoRow[] = [];
  if (isServerHls) {
    push(outputRows, "输出容器", formatContainer(output?.container));
    push(
      outputRows,
      "视频",
      joinParts([
        formatCodec(output?.videoCodec),
        output?.videoCodec && source?.video?.codec
          && output.videoCodec.toLowerCase() !== source.video.codec.toLowerCase()
          ? `源 ${formatCodec(source.video.codec)}`
          : null,
        output?.videoCodec ? null : "复制源",
      ]),
    );
    push(outputRows, "视频码率", formatBitrate(output?.videoBitrate));
    push(
      outputRows,
      "音频",
      joinParts([
        formatCodec(output?.audioCodec),
        output?.audioCodec && source?.audio?.codec
          && output.audioCodec.toLowerCase() !== source.audio.codec.toLowerCase()
          ? `源 ${formatCodec(source.audio.codec)}`
          : null,
        output?.audioCodec ? null : "复制源",
      ]),
    );
    push(outputRows, "音频码率", formatBitrate(output?.audioBitrate));
    push(outputRows, "转码档位", tierLabel(server?.tier ?? tier));
    if (!server) {
      push(outputRows, "统计数据", input.serverError ?? "正在读取服务端统计");
    }
  } else if (plan === "DIRECT") {
    push(outputRows, "输出容器", formatContainer(output?.container));
    push(outputRows, "视频", "直放，原始码流");
    push(outputRows, "音频", "直放，原始码流");
    push(outputRows, "转码", "未转码（服务端只读取字节）");
  } else {
    push(outputRows, "输出", input.serverError ?? "等待播放计划");
  }

  const liveRows: PlayerInfoRow[] = [];
  if (isServerHls) {
    push(liveRows, "编码帧率", formatFrameRate(progress?.fps));
    push(liveRows, "编码速度", formatSpeed(progress?.speed));
    push(
      liveRows,
      "输出码率",
      formatKbps(progress?.latestSegmentBitrateKbps ?? progress?.bitrateKbps),
    );
    push(liveRows, "平均码率", formatKbps(progress?.averageBitrateKbps));
    push(
      liveRows,
      "已输出",
      formatBytes(progress?.directoryBytes ?? progress?.totalSize),
    );
    push(
      liveRows,
      "丢帧 / 重复",
      progress ? `${progress.droppedFrames ?? 0} / ${progress.duplicatedFrames ?? 0}` : null,
    );
    push(liveRows, "编码位置", formatDuration(progress?.outTimeSeconds ?? null));
    push(liveRows, "编码状态", progress ? (progress.finished ? "已结束" : "编码中") : "等待 FFmpeg 首个进度报告");
    push(liveRows, "数据更新", formatAge(progress?.updatedAt, nowMs));
    if (input.serverError) push(liveRows, "统计请求", input.serverError);
  } else if (plan === "DIRECT") {
    push(liveRows, "推流方式", "Range 直读（服务端不处理）");
    push(liveRows, "推流码率", formatBitrate(source?.bitrate));
    push(liveRows, "服务端编码", "未启用（直放）");
    push(liveRows, "数据更新", "实时读取");
  } else {
    push(liveRows, "推流方式", null);
  }
  push(liveRows, "客户端帧率", formatFrameRate(client?.frameRate));
  push(
    liveRows,
    "解码帧 / 丢帧",
    client && client.totalFrames !== null
      ? `${client.totalFrames} / ${client.droppedFrames ?? 0}`
      : null,
  );
  push(liveRows, "缓冲", client?.bufferSeconds !== null && client?.bufferSeconds !== undefined
    ? `${client.bufferSeconds.toFixed(1)} 秒`
    : null);
  push(liveRows, "分辨率", client?.resolution);
  push(liveRows, "带宽估算", formatBitrate(client?.bandwidthBitsPerSecond));
  push(liveRows, "分片码率", formatBitrate(client?.levelBitrate));
  push(liveRows, "客户端处理", formatSpeed(client?.clientSpeedX));

  return [
    { title: "播放", rows: playbackRows },
    { title: "来源", rows: sourceRows },
    { title: "输出", rows: outputRows },
    { title: "实时", rows: liveRows },
  ];
}

export function playerInfoCopyText(groups: PlayerInfoGroup[]): string {
  return groups
    .map((group) => [`【${group.title}】`, ...group.rows.map((row) => `${row.label}：${row.value}`)].join("\n"))
    .join("\n\n");
}
