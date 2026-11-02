#!/usr/bin/env node
/**
 * Web playback compatibility matrix: sample generator.
 *
 * Generates a small, representative media corpus with the locally installed `ffmpeg` so the
 * compatibility matrix can be re-run against a test server without hand-picking files. Every sample
 * follows the Lux directory convention `Movies/<名称 (年份)>/<名称 (年份)>.<ext>` and uses only the
 * `.mkv` / `.mp4` containers the scanner accepts.
 *
 * The corpus is intentionally *not* a full cartesian product: video features (codec, bit depth,
 * profile, container, interlace, VFR, rotation, HDR, 4K ...) are paired with a known-good audio
 * track, while audio features (codec, channel layout, multiple tracks, no track) are paired with a
 * known-good video track. A handful of samples combine the exact codec pairs that already produced
 * field bugs (MPEG-4 Part 2 + AC-3, 4K HEVC + TrueHD, HEVC + DTS ...).
 *
 * Combinations that the local ffmpeg cannot produce are recorded in `manifest.json` under
 * `skipped` with a reason instead of being silently dropped.
 *
 * Usage:
 *   node scripts/playback-matrix/generate-samples.mjs [options]
 *
 * Options:
 *   --out <dir>        Output root (default: <script dir>/samples). Generated files are gitignored.
 *   --duration <sec>   Default sample duration (5-10s, default 5).
 *   --only <substr>    Only build samples whose title or group contains <substr> (repeatable).
 *   --force            Re-encode even when the target file already exists.
 *   --list             Print the plan and the skip list, generate nothing.
 *   --json             Machine-readable summary on stdout instead of the human summary.
 *   --help             Show this help.
 *
 * Exit codes: 0 = at least one sample generated (skips allowed), 1 = nothing generated,
 *             2 = usage or missing ffmpeg/ffprobe.
 */

import { spawn } from "node:child_process";
import { mkdir, rm, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const DEFAULT_OUT = path.join(SCRIPT_DIR, "samples");
const SAMPLE_YEAR = 2024;
const MIN_DURATION = 5;
const MAX_DURATION = 10;

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

function usage() {
  return `用法：node scripts/playback-matrix/generate-samples.mjs [options]

  --out <dir>        样本输出根目录（默认 scripts/playback-matrix/samples）
  --duration <sec>   默认样本时长，5-10 秒（默认 5）
  --only <substr>    只生成标题或分组包含该子串的样本，可重复
  --force            目标文件已存在时也重新编码
  --list             只打印计划与跳过项，不生成
  --json             stdout 输出 JSON 摘要
  --help             显示本帮助

输出结构：<out>/Movies/<名称 (年份)>/<名称 (年份)>.mkv|.mp4
清单文件：<out>/manifest.json`;
}

function parseArgs(argv) {
  const options = {
    out: DEFAULT_OUT,
    duration: "5",
    only: [],
    force: false,
    list: false,
    json: false,
    help: false,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    switch (arg) {
      case "--out":
        options.out = argv[index + 1] ?? "";
        index += 1;
        break;
      case "--duration":
        options.duration = argv[index + 1] ?? "";
        index += 1;
        break;
      case "--only":
        options.only.push(argv[index + 1] ?? "");
        index += 1;
        break;
      case "--force":
        options.force = true;
        break;
      case "--list":
        options.list = true;
        break;
      case "--json":
        options.json = true;
        break;
      case "--help":
      case "-h":
        options.help = true;
        break;
      default:
        throw new UsageError(`未知参数：${arg}`);
    }
  }
  if (!options.out) throw new UsageError("--out 需要一个目录");
  const duration = Number(options.duration);
  if (!Number.isFinite(duration) || duration < MIN_DURATION || duration > MAX_DURATION) {
    throw new UsageError(`--duration 必须在 ${MIN_DURATION}-${MAX_DURATION} 秒之间`);
  }
  options.durationSeconds = Math.round(duration * 10) / 10;
  options.out = path.resolve(options.out);
  if (options.only.some((value) => !value)) throw new UsageError("--only 需要一个子串");
  return options;
}

class UsageError extends Error {}

// ---------------------------------------------------------------------------
// Process helpers
// ---------------------------------------------------------------------------

function runProcess(command, args) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk) => {
      stdout += chunk;
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk;
    });
    child.on("error", reject);
    child.on("close", (code, signal) => {
      resolve({ code: code ?? -1, signal, stdout, stderr });
    });
  });
}

function tail(value, limit = 1200) {
  const text = String(value ?? "").trim();
  return text.length > limit ? `…${text.slice(-limit)}` : text;
}

async function ffmpegVersion(binary) {
  const result = await runProcess(binary, ["-hide_banner", "-version"]);
  if (result.code !== 0) {
    throw new UsageError(`无法运行 ${binary} -version：${tail(result.stderr, 300)}`);
  }
  return result.stdout.split("\n")[0]?.trim() ?? binary;
}

/** Parses `ffmpeg -encoders` into `name -> flags` (e.g. `libx265 -> V....D`). */
async function ffmpegEncoders(binary) {
  const result = await runProcess(binary, ["-hide_banner", "-encoders"]);
  if (result.code !== 0) {
    throw new UsageError(`无法运行 ${binary} -encoders：${tail(result.stderr, 300)}`);
  }
  const encoders = new Map();
  for (const line of result.stdout.split("\n")) {
    const match = line.match(/^\s([A-Z.]{6})\s(\S+)\s+(.*)$/);
    if (match) encoders.set(match[2], { flags: match[1], description: match[3].trim() });
  }
  return encoders;
}

async function ffprobeJson(binary, file) {
  const result = await runProcess(binary, [
    "-v",
    "error",
    "-print_format",
    "json",
    "-show_format",
    "-show_streams",
    file,
  ]);
  if (result.code !== 0) throw new Error(`ffprobe 失败：${tail(result.stderr, 400)}`);
  return JSON.parse(result.stdout);
}

// ---------------------------------------------------------------------------
// Encoder catalog
// ---------------------------------------------------------------------------

const X264_PROFILE = {
  baseline: "baseline",
  main: "main",
  high: "high",
  high10: "high10",
  high422: "high422",
  high444: "high444",
};

/**
 * Audio codec definitions. `channels` is the maximum the local encoder accepts; combinations above
 * it become `skipped` entries instead of failing the whole run.
 */
const AUDIO_CODECS = {
  aac: {
    encoder: "aac",
    maxChannels: 8,
    bitrate: (channels) => (channels > 2 ? "320k" : "128k"),
  },
  mp3: { encoder: "libmp3lame", maxChannels: 2, bitrate: () => "192k" },
  mp2: { encoder: "mp2", maxChannels: 2, bitrate: () => "192k" },
  ac3: { encoder: "ac3", maxChannels: 6, bitrate: () => "384k" },
  eac3: { encoder: "eac3", maxChannels: 6, bitrate: () => "384k" },
  dts: { encoder: "dca", maxChannels: 6, bitrate: () => "768k" },
  truehd: { encoder: "truehd", maxChannels: 8, bitrate: null },
  flac: { encoder: "flac", maxChannels: 8, bitrate: null },
  opus: { encoder: "libopus", maxChannels: 8, bitrate: (channels) => (channels > 2 ? "320k" : "128k") },
  vorbis: { encoder: "vorbis", maxChannels: 2, bitrate: () => "160k" },
  "pcm-s16le": { encoder: "pcm_s16le", maxChannels: 8, bitrate: null },
  "pcm-s24le": { encoder: "pcm_s24le", maxChannels: 8, bitrate: null },
  alac: { encoder: "alac", maxChannels: 8, bitrate: null },
  wma: { encoder: "wmav2", maxChannels: 2, bitrate: () => "192k" },
};

// ---------------------------------------------------------------------------
// Sample plan
// ---------------------------------------------------------------------------

/**
 * A sample spec:
 * {
 *   title, year, group, container: "mkv" | "mp4",
 *   video: { key, encoder, pixFmt, profile?, size?, fps?, args?, hdr?, tag? },
 *   audio: [{ codec, channels }],
 *   subtitles: [{ codec: "srt"|"ass", title?, language? }]  // never on mp4 (converted to mov_text)
 *   vfr?, rotate?, note?, duration?
 * }
 *
 * `title` is also the file/directory name (without the year) and must stay descriptive: the matrix
 * report reads codecs from ffprobe output, but a human reading the NAS folder should not need it.
 */

function x264(profile, pixFmt, extra = {}) {
  return {
    encoder: "libx264",
    profile: X264_PROFILE[profile] ?? profile,
    pixFmt,
    args: ["-preset", "veryfast", "-crf", profile === "baseline" ? "30" : "28"],
    ...extra,
  };
}

function x265(pixFmt, extra = {}) {
  return {
    encoder: "libx265",
    pixFmt,
    args: ["-preset", "ultrafast", "-crf", "32", "-x265-params", "log-level=error"],
    ...extra,
  };
}

const H264_HIGH = x264("high", "yuv420p");
const AAC_STEREO = [{ codec: "aac", channels: 2 }];

function audio(codec, channels) {
  return { codec, channels };
}

const SAMPLES = [
  // --- video feature coverage (known-good AAC stereo audio) -----------------
  {
    title: "h264-baseline-aac-stereo",
    group: "video",
    video: x264("baseline", "yuv420p"),
    note: "H.264 Constrained Baseline 8-bit",
  },
  {
    title: "h264-main-aac-stereo",
    group: "video",
    video: x264("main", "yuv420p"),
    note: "H.264 Main 8-bit",
  },
  {
    title: "h264-high-aac-stereo",
    group: "video",
    video: H264_HIGH,
    note: "H.264 High 8-bit，音频覆盖的基准样本",
  },
  {
    title: "h264-high10-aac-stereo",
    group: "video",
    video: x264("high10", "yuv420p10le"),
    note: "H.264 High 10（10-bit）",
  },
  {
    title: "h264-high422-aac-stereo",
    group: "video",
    video: x264("high422", "yuv422p"),
    note: "H.264 High 4:2:2",
  },
  {
    title: "h264-high444-aac-stereo",
    group: "video",
    video: x264("high444", "yuv444p"),
    note: "H.264 High 4:4:4 Predictive",
  },
  {
    title: "h264-high-aac-stereo-mp4",
    group: "video",
    container: "mp4",
    video: H264_HIGH,
    note: "H.264 High + AAC 的 MP4 容器基线",
  },
  {
    title: "hevc-main-aac-stereo",
    group: "video",
    video: x265("yuv420p"),
    note: "HEVC Main 8-bit",
  },
  {
    title: "hevc-main10-aac-stereo",
    group: "video",
    video: x265("yuv420p10le"),
    note: "HEVC Main10（10-bit）",
  },
  {
    title: "hevc-main10-aac-stereo-mp4",
    group: "video",
    container: "mp4",
    video: x265("yuv420p10le", { tag: "hvc1" }),
    note: "HEVC Main10 的 MP4 容器（hvc1 tag）",
  },
  {
    title: "vp8-aac-stereo",
    group: "video",
    video: {
      encoder: "libvpx",
      pixFmt: "yuv420p",
      args: ["-b:v", "300k", "-crf", "30", "-deadline", "realtime", "-cpu-used", "8"],
    },
    note: "VP8 8-bit",
  },
  {
    title: "vp9-aac-stereo",
    group: "video",
    video: {
      encoder: "libvpx-vp9",
      pixFmt: "yuv420p",
      args: ["-b:v", "300k", "-crf", "35", "-deadline", "realtime", "-cpu-used", "8"],
    },
    note: "VP9 8-bit",
  },
  {
    title: "vp9-10bit-aac-stereo",
    group: "video",
    video: {
      encoder: "libvpx-vp9",
      pixFmt: "yuv420p10le",
      args: ["-b:v", "300k", "-crf", "35", "-deadline", "realtime", "-cpu-used", "8"],
    },
    note: "VP9 Profile 2（10-bit）",
  },
  {
    title: "av1-aac-stereo",
    group: "video",
    video: {
      encoder: "libsvtav1",
      pixFmt: "yuv420p",
      args: ["-preset", "12", "-crf", "45"],
    },
    note: "AV1 Main 8-bit（SVT-AV1）",
  },
  {
    title: "av1-10bit-aac-stereo",
    group: "video",
    video: {
      encoder: "libsvtav1",
      pixFmt: "yuv420p10le",
      args: ["-preset", "12", "-crf", "45"],
    },
    note: "AV1 Main 10-bit",
  },
  {
    title: "mpeg2-aac-stereo",
    group: "video",
    video: { encoder: "mpeg2video", pixFmt: "yuv420p", args: ["-q:v", "5"] },
    note: "MPEG-2 Video",
  },
  {
    title: "mpeg4-part2-aac-stereo",
    group: "video",
    video: { encoder: "mpeg4", pixFmt: "yuv420p", args: ["-q:v", "5"] },
    note: "MPEG-4 Part 2（原生 MKV 有声无画的已知疑似点）",
  },
  {
    title: "mjpeg-aac-stereo",
    group: "video",
    video: { encoder: "mjpeg", pixFmt: "yuvj420p", args: ["-q:v", "5"] },
    note: "Motion JPEG",
  },
  {
    title: "prores-aac-stereo",
    group: "video",
    video: {
      encoder: "prores_ks",
      pixFmt: "yuv422p10le",
      args: ["-profile:v", "2"],
    },
    note: "Apple ProRes 422 Standard",
  },
  {
    title: "h264-high-interlaced-aac-stereo",
    group: "video",
    video: x264("high", "yuv420p", {
      fps: 25,
      args: ["-preset", "veryfast", "-crf", "28", "-flags", "+ilme+ildct", "-field_order", "tt"],
      interlaced: true,
    }),
    note: "H.264 隔行（top field first）",
  },
  {
    title: "h264-high-60fps-aac-stereo",
    group: "video",
    video: x264("high", "yuv420p", { fps: 60 }),
    note: "H.264 60fps",
  },
  {
    title: "h264-high-vfr-aac-stereo",
    group: "video",
    vfr: true,
    video: x264("high", "yuv420p"),
    note: "H.264 可变帧率（24fps 段 + 60fps 段）",
  },
  {
    title: "h264-high-1918x1078-aac-stereo",
    group: "video",
    video: x264("high", "yuv420p", { size: "1918x1078" }),
    note: "非标准/近似奇数的编码尺寸 1918x1078",
  },
  {
    title: "h264-high-rotation-aac-stereo",
    group: "video",
    rotate: 90,
    video: x264("high", "yuv420p"),
    note: "容器内 90° 显示矩阵旋转元数据（编码尺寸仍为 320x180）",
  },
  {
    title: "hevc-main10-hdr10-aac-stereo",
    group: "video",
    video: x265("yuv420p10le", {
      hdr: true,
      args: [
        "-preset",
        "ultrafast",
        "-crf",
        "32",
        "-x265-params",
        "log-level=error:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:range=limited:master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):max-cll=1000,400",
      ],
    }),
    note: "HDR10：BT.2020 / PQ / 10-bit",
  },
  {
    title: "hevc-main10-4k-aac-stereo",
    group: "video",
    video: x265("yuv420p10le", { size: "3840x2160" }),
    note: "4K HEVC Main10（remux 字节错位的已知疑似点）",
  },

  // --- audio feature coverage (known-good H.264 High video) -----------------
  {
    title: "h264-high-aac-5.1",
    group: "audio",
    audio: [audio("aac", 6)],
    note: "AAC 5.1",
  },
  {
    title: "h264-high-aac-mono",
    group: "audio",
    audio: [audio("aac", 1)],
    note: "AAC mono",
  },
  {
    title: "h264-high-mp3-stereo",
    group: "audio",
    audio: [audio("mp3", 2)],
    note: "MP3 立体声",
  },
  {
    title: "h264-high-ac3-stereo",
    group: "audio",
    audio: [audio("ac3", 2)],
    note: "AC-3 立体声（Edge 无声的已知疑似点）",
  },
  {
    title: "h264-high-ac3-5.1",
    group: "audio",
    audio: [audio("ac3", 6)],
    note: "AC-3 5.1",
  },
  {
    title: "h264-high-eac3-5.1",
    group: "audio",
    audio: [audio("eac3", 6)],
    note: "E-AC-3 5.1",
  },
  {
    title: "h264-high-dts-5.1",
    group: "audio",
    audio: [audio("dts", 6)],
    note: "DTS core 5.1（无法生成 DTS-HD MA）",
  },
  {
    title: "h264-high-truehd-5.1",
    group: "audio",
    audio: [audio("truehd", 6)],
    note: "TrueHD 5.1（实验性编码器）",
  },
  {
    title: "h264-high-flac-5.1",
    group: "audio",
    audio: [audio("flac", 6)],
    note: "FLAC 5.1",
  },
  {
    title: "h264-high-flac-7.1",
    group: "audio",
    audio: [audio("flac", 8)],
    note: "FLAC 7.1",
  },
  {
    title: "h264-high-opus-5.1",
    group: "audio",
    audio: [audio("opus", 6)],
    note: "Opus 5.1",
  },
  {
    title: "h264-high-vorbis-stereo",
    group: "audio",
    audio: [audio("vorbis", 2)],
    note: "Vorbis 立体声（实验性编码器；本地编码器不支持多声道）",
  },
  {
    title: "h264-high-pcm-s16le-stereo",
    group: "audio",
    audio: [audio("pcm-s16le", 2)],
    note: "PCM s16le 立体声",
  },
  {
    title: "h264-high-pcm-s24le-5.1",
    group: "audio",
    audio: [audio("pcm-s24le", 6)],
    note: "PCM s24le 5.1",
  },
  {
    title: "h264-high-alac-5.1-mp4",
    group: "audio",
    container: "mp4",
    audio: [audio("alac", 6)],
    note: "ALAC 5.1（Matroska 不支持 ALAC，使用 MP4）",
  },
  {
    title: "h264-high-wma-stereo",
    group: "audio",
    audio: [audio("wma", 2)],
    note: "WMA v2 立体声",
  },
  {
    title: "h264-high-mp2-stereo",
    group: "audio",
    audio: [audio("mp2", 2)],
    note: "MP2 立体声",
  },
  {
    title: "h264-high-noaudio",
    group: "audio",
    audio: [],
    note: "无音轨（视频-only）",
  },

  // --- multi-track audio ---------------------------------------------------
  {
    title: "h264-high-aac-ac3-multiaudio",
    group: "audio",
    audio: [audio("aac", 2), audio("ac3", 6)],
    note: "双音轨：AAC 2.0 + AC-3 5.1",
  },
  {
    title: "hevc-main10-flac-opus-multiaudio",
    group: "audio",
    video: x265("yuv420p10le"),
    audio: [audio("flac", 6), audio("opus", 2)],
    note: "双音轨：FLAC 5.1 + Opus 2.0",
  },

  // --- subtitles -----------------------------------------------------------
  {
    title: "h264-high-srt-aac-stereo",
    group: "subtitles",
    subtitles: [{ codec: "srt", title: "SRT 中文", language: "chi" }],
    note: "内嵌 SRT 字幕",
  },
  {
    title: "h264-high-ass-aac-stereo",
    group: "subtitles",
    subtitles: [{ codec: "ass", title: "ASS 中文", language: "chi" }],
    note: "内嵌 ASS 字幕",
  },
  {
    title: "h264-high-multisub-aac-stereo",
    group: "subtitles",
    subtitles: [
      { codec: "srt", title: "SRT 中文", language: "chi" },
      { codec: "ass", title: "ASS 英文", language: "eng", alternate: true },
    ],
    note: "内嵌双字幕轨（SRT + ASS）",
  },
  {
    title: "h264-high-movtext-aac-stereo-mp4",
    group: "subtitles",
    container: "mp4",
    subtitles: [{ codec: "mov_text", title: "MovText", language: "chi" }],
    note: "MP4 mov_text 字幕",
  },

  // --- known-bug codec pairs ----------------------------------------------
  {
    title: "mpeg4-part2-ac3-5.1",
    group: "combos",
    video: { encoder: "mpeg4", pixFmt: "yuv420p", args: ["-q:v", "5"] },
    audio: [audio("ac3", 6)],
    note: "MPEG-4 Part 2 + AC-3 5.1：原生 MKV 有声无画的疑似组合",
  },
  {
    title: "hevc-main10-4k-truehd-5.1",
    group: "combos",
    video: x265("yuv420p10le", { size: "3840x2160" }),
    audio: [audio("truehd", 6)],
    note: "4K HEVC + TrueHD 5.1：remux 字节错位的疑似组合",
  },
  {
    title: "hevc-main10-dts-5.1",
    group: "combos",
    video: x265("yuv420p10le"),
    audio: [audio("dts", 6)],
    note: "HEVC + DTS 5.1：静默音频回退的疑似组合",
  },
  {
    title: "h264-high-eac3-5.1-mp4",
    group: "combos",
    container: "mp4",
    audio: [audio("eac3", 6)],
    note: "MP4 容器 + E-AC-3 5.1",
  },
].map((sample) => ({
  year: SAMPLE_YEAR,
  container: "mkv",
  audio: AAC_STEREO,
  subtitles: [],
  ...sample,
  video: sample.video ?? H264_HIGH,
}));

/** Coverage that cannot be produced with the local ffmpeg, always reported. */
const DECLARED_SKIPS = [
  {
    title: "vc1",
    group: "video",
    reason: "本机 ffmpeg 没有 VC-1 编码器（仅解码器）；无法生成 VC-1 样本。",
  },
  {
    title: "pgs",
    group: "subtitles",
    reason: "ffmpeg 没有 PGS 位图字幕编码器；PGS 字幕轨道未覆盖，需使用现成蓝光样本。",
  },
  {
    title: "he-aac",
    group: "audio",
    reason: "本机 ffmpeg 的 aac 编码器仅支持 AAC-LC；没有 libfdk_aac，无法生成 HE-AAC。",
  },
  {
    title: "dts-hd-ma",
    group: "audio",
    reason: "ffmpeg 的 dca 编码器只能生成 DTS core，没有 DTS-HD MA 无损编码器。",
  },
  {
    title: "ac3-7.1",
    group: "audio",
    reason: "AC-3 规范只到 5.1；7.1 已由 FLAC/PCM/Opus/TrueHD 样本覆盖。",
  },
  {
    title: "eac3-7.1",
    group: "audio",
    reason: "本机 eac3 编码器最多 6 声道；7.1 已由 FLAC/PCM/Opus/TrueHD 样本覆盖。",
  },
  {
    title: "vorbis-5.1",
    group: "audio",
    reason: "本机原生 vorbis 编码器只支持 2 声道。",
  },
];

// ---------------------------------------------------------------------------
// Generation
// ---------------------------------------------------------------------------

function mediaArgs(spec) {
  const video = spec.video;
  const fps = video.fps ?? 24;
  const size = video.size ?? "320x180";
  const duration = spec.duration ?? spec.defaultDuration;
  return {
    fps,
    size,
    duration,
    videoInput: ["-f", "lavfi", "-i", `testsrc2=size=${size}:rate=${fps}:duration=${duration}`],
  };
}

function subtitlePath(assetsDir, index, codec, alternate = false) {
  // The alternate track is only shipped as SRT; ffmpeg converts it to ASS when asked.
  if (alternate) return path.join(assetsDir, "subtitle-2.srt");
  return path.join(assetsDir, codec === "ass" ? "subtitle.ass" : "subtitle.srt");
}

function buildOutputArgs(spec, { assetsDir, outFile }) {
  const { duration } = mediaArgs(spec);
  const args = [];
  const tracks = spec.audio;
  const subtitles = spec.subtitles;

  args.push("-map", "0:v:0");
  tracks.forEach((_, index) => args.push("-map", `${index + 1}:a:0`));
  subtitles.forEach((_, index) => args.push("-map", `${tracks.length + index + 1}:s:0`));

  const video = spec.video;
  args.push("-c:v", video.encoder);
  if (video.profile) args.push("-profile:v", video.profile);
  if (video.tag) args.push("-tag:v", video.tag);
  if (video.pixFmt) args.push("-pix_fmt", video.pixFmt);
  args.push(...(video.args ?? []));

  tracks.forEach((track, index) => {
    const definition = AUDIO_CODECS[track.codec];
    args.push("-c:a:" + index, definition.encoder);
    args.push("-ac:a:" + index, String(track.channels));
    const bitrate = definition.bitrate?.(track.channels);
    if (bitrate) args.push(`-b:a:${index}`, bitrate);
    args.push(`-metadata:s:a:${index}`, `language=${index === 0 ? "eng" : "jpn"}`);
    args.push(`-metadata:s:a:${index}`, `title=${track.codec.toUpperCase()} ${channelLabel(track.channels)}`);
  });
  if (tracks.length === 0) args.push("-an");
  if (tracks.some((track) => ["dts", "truehd", "vorbis"].includes(track.codec))) {
    args.unshift("-strict", "-2");
  }

  subtitles.forEach((subtitle, index) => {
    // `mov_text` is a track-level conversion of the SRT input; ASS tracks stay ASS inside MKV.
    args.push(`-c:s:${index}`, subtitle.codec === "mov_text" ? "mov_text" : subtitle.codec);
    args.push(`-metadata:s:s:${index}`, `language=${subtitle.language ?? "chi"}`);
    if (subtitle.title) args.push(`-metadata:s:s:${index}`, `title=${subtitle.title}`);
  });

  if (spec.container === "mp4") args.push("-movflags", "+faststart");
  args.push("-t", String(duration), outFile);
  return args;
}

function channelLabel(channels) {
  if (channels === 1) return "mono";
  if (channels === 2) return "stereo";
  if (channels === 6) return "5.1";
  if (channels === 8) return "7.1";
  return `${channels}ch`;
}

function audioInputs(spec) {
  const { duration } = mediaArgs(spec);
  return spec.audio.map((track, index) => {
    const frequency = 440 + index * 220;
    return [
      "-f", "lavfi", "-i",
      `sine=frequency=${frequency}:sample_rate=48000:duration=${duration}`,
    ];
  });
}

function subtitleInputs(spec, assetsDir) {
  return spec.subtitles.map((subtitle, index) => {
    const codec = subtitle.codec === "mov_text" ? "srt" : subtitle.codec;
    return ["-i", subtitlePath(assetsDir, index, codec, Boolean(subtitle.alternate))];
  });
}

function ffmpegArgsForSample(spec, context) {
  const { videoInput } = mediaArgs(spec);
  const { assetsDir, outFile } = context;
  const args = ["-y", "-hide_banner", "-loglevel", "error", "-nostdin", ...videoInput];
  for (const input of audioInputs(spec)) args.push(...input);
  for (const input of subtitleInputs(spec, assetsDir)) args.push(...input);
  args.push(...buildOutputArgs(spec, context));
  return args;
}

async function ensureSubtitleAssets(assetsDir) {
  await mkdir(assetsDir, { recursive: true });
  const srt = `1
00:00:00,500 --> 00:00:02,400
Lux 播放矩阵字幕：第一条

2
00:00:02,800 --> 00:00:04,600
Lux 播放矩阵字幕：第二条
`;
  const altSrt = `1
00:00:00,500 --> 00:00:02,400
Lux second subtitle track

2
00:00:02,800 --> 00:00:04,600
Second cue on the alternate track
`;
  const ass = `[Script Info]
ScriptType: v4.00+
PlayResX: 320
PlayResY: 180
WrapStyle: 0

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding
Style: Default,Arial,20,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,0,2,10,10,10,1

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:00.50,0:00:02.40,Default,,0,0,0,,Lux ASS字幕第一行
Dialogue: 0,0:00:02.80,0:00:04.60,Default,,0,0,0,,Lux ASS字幕第二行
`;
  await writeFile(path.join(assetsDir, "subtitle.srt"), srt, "utf8");
  await writeFile(path.join(assetsDir, "subtitle-2.srt"), altSrt, "utf8");
  await writeFile(path.join(assetsDir, "subtitle.ass"), ass, "utf8");
}

function ffmpegBinary() {
  return process.env.FFMPEG || "ffmpeg";
}

function ffprobeBinary() {
  return process.env.FFPROBE || "ffprobe";
}

async function encodeVfrSample(spec, context, encoders) {
  const { outFile, workDir } = context;
  const { duration } = mediaArgs(spec);
  const segmentDuration = Math.max(1, Math.round(duration / 2));
  const segmentSeconds = [segmentDuration, duration - segmentDuration];
  const segments = [];
  for (const [index, seconds] of segmentSeconds.entries()) {
    const segmentFile = path.join(workDir, `${spec.title}-segment${index + 1}.mkv`);
    const fps = index === 0 ? 24 : 60;
    const args = [
      "-y", "-hide_banner", "-loglevel", "error", "-nostdin",
      "-f", "lavfi", "-i", `testsrc2=size=${spec.video.size ?? "320x180"}:rate=${fps}:duration=${seconds}`,
      "-c:v", spec.video.encoder,
      ...(spec.video.profile ? ["-profile:v", spec.video.profile] : []),
      "-pix_fmt", spec.video.pixFmt,
      ...(spec.video.args ?? []),
      "-t", String(seconds),
      segmentFile,
    ];
    await runFfmpeg(args, `VFR 片段 ${index + 1}`);
    segments.push(segmentFile);
  }
  const listFile = path.join(workDir, `${spec.title}-concat.txt`);
  await writeFile(
    listFile,
    segments.map((segment) => `file '${segment.replaceAll("'", "'\\''")}'`).join("\n") + "\n",
    "utf8",
  );
  const joinedFile = path.join(workDir, `${spec.title}-joined.mkv`);
  await runFfmpeg(
    ["-y", "-hide_banner", "-loglevel", "error", "-nostdin", "-f", "concat", "-safe", "0", "-i", listFile, "-c", "copy", joinedFile],
    "VFR 片段拼接",
  );
  const { duration: totalDuration } = mediaArgs(spec);
  const args = [
    "-y", "-hide_banner", "-loglevel", "error", "-nostdin",
    "-i", joinedFile,
    "-f", "lavfi", "-i", `sine=frequency=440:sample_rate=48000:duration=${totalDuration}`,
    "-map", "0:v:0", "-map", "1:a:0",
    "-c:v", "copy",
    "-c:a", "aac", "-b:a", "128k", "-ac:a", "2",
    "-metadata:s:a:0", "language=eng",
    "-t", String(totalDuration),
    outFile,
  ];
  assertEncoders(spec, encoders);
  await runFfmpeg(args, "VFR 封装");
}

async function encodeRotatedSample(spec, context) {
  const { outFile, workDir } = context;
  const baseFile = path.join(workDir, `${spec.title}-base.mkv`);
  const baseSpec = { ...spec, rotate: null };
  const baseArgs = ffmpegArgsForSample(baseSpec, { ...context, outFile: baseFile });
  await runFfmpeg(baseArgs, "旋转样本基础编码");
  await runFfmpeg(
    [
      "-y", "-hide_banner", "-loglevel", "error", "-nostdin",
      "-display_rotation", String(spec.rotate),
      "-i", baseFile,
      "-map", "0", "-c", "copy",
      outFile,
    ],
    "写入旋转显示矩阵",
  );
}

function assertEncoders(spec, encoders) {
  const required = [spec.video.encoder, ...spec.audio.map((track) => AUDIO_CODECS[track.codec].encoder)];
  for (const encoder of required) {
    if (!encoders.has(encoder)) {
      throw new SkipError(`本机 ffmpeg 缺少编码器 ${encoder}`);
    }
  }
}

class SkipError extends Error {}

async function runFfmpeg(args, label) {
  const result = await runProcess(ffmpegBinary(), args);
  if (result.code !== 0) {
    throw new Error(`${label}：ffmpeg 退出码 ${result.code}\n${tail(result.stderr)}`);
  }
  return result;
}

function summarizeProbe(probe) {
  const streams = probe.streams ?? [];
  const video = streams.filter((stream) => stream.codec_type === "video").map((stream) => ({
    codec: stream.codec_name,
    profile: stream.profile ?? null,
    pixFmt: stream.pix_fmt ?? null,
    width: stream.width,
    height: stream.height,
    fps: stream.avg_frame_rate ?? null,
    fieldOrder: stream.field_order ?? null,
    rotation: stream.side_data_list?.find((item) => item.rotation !== undefined)?.rotation ?? null,
    colorPrimaries: stream.color_primaries ?? null,
    colorTransfer: stream.color_transfer ?? null,
    colorSpace: stream.color_space ?? null,
  }));
  const audio = streams.filter((stream) => stream.codec_type === "audio").map((stream) => ({
    codec: stream.codec_name,
    profile: stream.profile ?? null,
    channels: stream.channels,
    channelLayout: stream.channel_layout ?? null,
    sampleRate: stream.sample_rate ?? null,
  }));
  const subtitles = streams.filter((stream) => stream.codec_type === "subtitle").map((stream) => ({
    codec: stream.codec_name,
    language: stream.tags?.language ?? null,
    title: stream.tags?.title ?? null,
  }));
  return {
    durationSeconds: Number(probe.format?.duration ?? 0),
    video,
    audio,
    subtitles,
  };
}

function sampleDirectory(spec) {
  const displayName = `${spec.title} (${spec.year})`;
  return displayName;
}

async function fileSize(file) {
  try {
    return (await stat(file)).size;
  } catch {
    return 0;
  }
}

function matchesOnly(spec, only) {
  if (only.length === 0) return true;
  const haystack = `${spec.title} ${spec.group}`.toLowerCase();
  return only.some((value) => haystack.includes(value.toLowerCase()));
}

async function main() {
  let options;
  try {
    options = parseArgs(process.argv.slice(2));
  } catch (error) {
    if (error instanceof UsageError) {
      console.error(`错误：${error.message}\n\n${usage()}`);
      return 2;
    }
    throw error;
  }
  if (options.help) {
    console.log(usage());
    return 0;
  }

  let ffmpegLine;
  let encoders;
  try {
    ffmpegLine = await ffmpegVersion(ffmpegBinary());
    encoders = await ffmpegEncoders(ffmpegBinary());
    await ffmpegVersion(ffprobeBinary());
  } catch (error) {
    console.error(`错误：${error.message}`);
    return 2;
  }

  const selected = SAMPLES.filter((spec) => matchesOnly(spec, options.only));
  if (options.list) {
    console.log(`ffmpeg：${ffmpegLine}`);
    console.log(`计划生成 ${selected.length} 个样本（输出：${options.out}）：`);
    for (const spec of selected) {
      const features = [];
      features.push(spec.video.encoder.replace(/^lib/, ""));
      if (spec.video.profile) features.push(spec.video.profile);
      if (spec.video.pixFmt) features.push(spec.video.pixFmt);
      if (spec.video.size) features.push(spec.video.size);
      if (spec.video.fps) features.push(`${spec.video.fps}fps`);
      if (spec.vfr) features.push("vfr");
      if (spec.rotate) features.push(`rotate=${spec.rotate}`);
      features.push(spec.audio.length ? spec.audio.map((track) => `${track.codec}/${track.channels}ch`).join("+") : "no-audio");
      if (spec.subtitles.length) features.push(spec.subtitles.map((subtitle) => subtitle.codec).join("+"));
      console.log(`  - ${spec.title}.${spec.container}  [${spec.group}]  ${features.join(", ")}`);
    }
    console.log(`声明但无法生成：${DECLARED_SKIPS.length} 项`);
    for (const skip of DECLARED_SKIPS) console.log(`  - ${skip.title}：${skip.reason}`);
    return 0;
  }

  await mkdir(options.out, { recursive: true });
  const assetsDir = path.join(options.out, ".assets");
  const workDir = path.join(options.out, ".work");
  await mkdir(workDir, { recursive: true });
  await ensureSubtitleAssets(assetsDir);

  const generated = [];
  const skipped = DECLARED_SKIPS.map((skip) => ({
    title: skip.title,
    group: skip.group,
    reason: skip.reason,
    declared: true,
  }));

  for (const spec of selected) {
    const directoryName = sampleDirectory(spec);
    const directory = path.join(options.out, "Movies", directoryName);
    const fileName = `${directoryName}.${spec.container}`;
    const outFile = path.join(directory, fileName);
    const relativePath = path.relative(options.out, outFile);
    const context = {
      assetsDir,
      workDir,
      outFile,
      defaultDuration: options.durationSeconds,
    };
    spec.defaultDuration = options.durationSeconds;

    const skippedEntry = {
      title: spec.title,
      group: spec.group,
      container: spec.container,
      reason: "",
      declared: false,
    };

    try {
      assertEncoders(spec, encoders);
      if (spec.container === "mp4" && spec.subtitles.some((subtitle) => subtitle.codec === "ass")) {
        throw new SkipError("MP4 容器无法直接封装 ASS 字幕；ASS 覆盖使用 MKV 样本。");
      }
      if (spec.container === "mkv" && spec.audio.some((track) => track.codec === "alac")) {
        throw new SkipError("Matroska muxer 不支持 ALAC；ALAC 覆盖使用 MP4 样本。");
      }
      await mkdir(directory, { recursive: true });
      const existingSize = await fileSize(outFile);
      if (!options.force && existingSize > 0) {
        skippedEntry.reused = true;
      } else {
        await rm(outFile, { force: true });
        if (spec.vfr) {
          await encodeVfrSample(spec, context, encoders);
        } else if (spec.rotate) {
          await encodeRotatedSample(spec, context);
        } else {
          await runFfmpeg(ffmpegArgsForSample(spec, context), spec.title);
        }
      }
      const probe = await ffprobeJson(ffprobeBinary(), outFile);
      const summary = summarizeProbe(probe);
      if (summary.video.length === 0) throw new Error("生成结果没有视频流");
      if (spec.audio.length > 0 && summary.audio.length !== spec.audio.length) {
        throw new Error(`音轨数量不符：期望 ${spec.audio.length}，实际 ${summary.audio.length}`);
      }
      if (summary.durationSeconds < options.durationSeconds - 0.5) {
        throw new Error(`时长过短：${summary.durationSeconds.toFixed(2)}s`);
      }
      if (spec.rotate && summary.video[0].rotation !== spec.rotate) {
        throw new Error(`旋转元数据未写入：${JSON.stringify(summary.video[0].rotation)}`);
      }
      const bytes = await fileSize(outFile);
      generated.push({
        title: spec.title,
        group: spec.group,
        container: spec.container,
        path: relativePath,
        bytes,
        durationSeconds: summary.durationSeconds,
        features: {
          video: spec.video.encoder,
          profile: spec.video.profile ?? null,
          pixFmt: spec.video.pixFmt ?? null,
          size: spec.video.size ?? "320x180",
          fps: spec.video.fps ?? 24,
          interlaced: Boolean(spec.video.interlaced),
          vfr: Boolean(spec.vfr),
          rotation: spec.rotate ?? null,
          hdr: Boolean(spec.video.hdr),
          audio: spec.audio.map((track) => ({ codec: track.codec, channels: track.channels })),
          subtitles: spec.subtitles.map((subtitle) => subtitle.codec),
        },
        streams: summary,
        note: spec.note ?? null,
        reused: Boolean(skippedEntry.reused),
      });
      console.log(`生成 ${relativePath}（${(bytes / 1024 / 1024).toFixed(2)} MB）`);
    } catch (error) {
      await rm(outFile, { force: true });
      skippedEntry.reason = error instanceof SkipError ? error.message : `生成失败：${error.message}`;
      skipped.push(skippedEntry);
      console.log(`跳过 ${spec.title}：${skippedEntry.reason}`);
    }
  }

  const totalBytes = generated.reduce((sum, sample) => sum + sample.bytes, 0);
  const manifest = {
    generatedAt: new Date().toISOString(),
    generator: "scripts/playback-matrix/generate-samples.mjs",
    ffmpeg: ffmpegLine,
    ffprobe: ffprobeBinary(),
    outDir: options.out,
    conventions: {
      layout: "Movies/<名称 (年份)>/<名称 (年份)>.<ext>",
      containers: ["mkv", "mp4"],
      durationSeconds: options.durationSeconds,
    },
    totals: {
      requested: selected.length,
      generated: generated.length,
      skipped: skipped.length,
      bytes: totalBytes,
    },
    coverage: coverageSummary(generated),
    samples: generated,
    skipped,
  };
  await writeFile(path.join(options.out, "manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`, "utf8");
  await rm(workDir, { recursive: true, force: true });

  const summary = {
    outDir: options.out,
    generated: generated.length,
    skipped: skipped.length,
    bytes: totalBytes,
    manifest: path.join(options.out, "manifest.json"),
  };
  if (options.json) {
    console.log(JSON.stringify(summary, null, 2));
  } else {
    console.log("");
    console.log(`完成：生成 ${generated.length} 个样本，跳过 ${skipped.length} 项，共 ${(totalBytes / 1024 / 1024).toFixed(1)} MB`);
    console.log(`清单：${summary.manifest}`);
    console.log(`把 ${options.out} 作为媒体库根目录同步到测试实例（脚本不负责 ssh）。`);
  }
  return generated.length > 0 ? 0 : 1;
}

function coverageSummary(generated) {
  const videoCodecs = new Set();
  const audioCodecs = new Set();
  const channelLayouts = new Set();
  const subtitleCodecs = new Set();
  const containers = new Set();
  const flags = new Set();
  for (const sample of generated) {
    const video = sample.streams.video[0];
    if (video?.codec) videoCodecs.add(video.codec);
    for (const audio of sample.streams.audio) {
      audioCodecs.add(audio.codec);
      if (audio.channels) channelLayouts.add(channelLabel(audio.channels));
    }
    for (const subtitle of sample.streams.subtitles) subtitleCodecs.add(subtitle.codec);
    containers.add(sample.container);
    if (sample.features.interlaced) flags.add("interlaced");
    if (sample.features.vfr) flags.add("vfr");
    if (sample.features.rotation) flags.add("rotation");
    if (sample.features.hdr) flags.add("hdr10");
    if (sample.features.size === "3840x2160") flags.add("4k");
    if (sample.features.fps === 60) flags.add("60fps");
    if (sample.features.size === "1918x1078") flags.add("odd-size");
  }
  return {
    videoCodecs: [...videoCodecs].sort(),
    audioCodecs: [...audioCodecs].sort(),
    channelLayouts: [...channelLayouts].sort(),
    subtitleCodecs: [...subtitleCodecs].sort(),
    containers: [...containers].sort(),
    flags: [...flags].sort(),
  };
}

try {
  process.exitCode = await main();
} catch (error) {
  console.error(`未预期的错误：${error?.stack ?? error}`);
  process.exitCode = 1;
}
