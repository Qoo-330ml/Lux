#!/usr/bin/env node
/**
 * Web playback compatibility matrix: runner.
 *
 * Logs into a Lux test instance with Playwright (the same `createRequire` trick as
 * `scripts/browser-smoke.mjs`, so the repository does not gain an npm dependency), walks every media
 * library, and plays every source serially in a real browser. For each source it records the server
 * plan, whether the client fell back, whether video frames and audio samples were really decoded, and
 * how long the first frame took. The output is `report.md` (human) and `report.json` (machine).
 *
 * This tool never runs in CI and never writes credentials to disk: the password is read from
 * `LUX_PASS` only, and console/URL details are stripped of query strings before they reach a report.
 *
 * Environment:
 *   BASE_URL / LUX_BASE_URL     Server root, default http://127.0.0.1:8097
 *   LUX_USER                    Test account (required)
 *   LUX_PASS                    Password (required, environment only)
 *   BROWSER                     chrome (default) | webkit | firefox | comma separated list
 *   CHROME_PATH                 Chrome executable; default /Applications/Google Chrome.app/...
 *   MATRIX_HEADED=1             Run with a visible window (needed for real GPU decode/HDR)
 *   MATRIX_TIMEOUT_MS           Per-sample wait budget, default 30000
 *   MATRIX_RELEASE_MS           Idle wait after about:blank, default 1500
 *   MATRIX_OUT                  Report directory, default scripts/playback-matrix/reports
 *   MATRIX_LIBRARY              Only libraries whose name contains this substring
 *   MATRIX_FILTER               Only item titles matching this regular expression
 *   MATRIX_MAX_ITEMS            Stop after N sources (debugging)
 *   MATRIX_SCREENSHOTS=0        Disable failure screenshots
 *   PLAYWRIGHT_MODULE_DIRS      Extra node module dirs to resolve playwright from
 *
 * Usage:
 *   LUX_USER=viewer LUX_PASS=... node scripts/playback-matrix/run-matrix.mjs
 *
 * Exit codes:
 *   0  every played sample was clean (skips are allowed)
 *   1  at least one ❌ failure
 *   2  usage/config/login/playwright error (no meaningful matrix produced)
 */

import { createRequire } from "node:module";
import { existsSync } from "node:fs";
import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const DEFAULT_BASE_URL = "http://127.0.0.1:8097";
const DEFAULT_REPORT_DIR = path.join(SCRIPT_DIR, "reports");
const DEFAULT_CHROME_PATH = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const STATUS_SYMBOL = { ok: "✅", warning: "⚠️", failed: "❌", skipped: "⏭️" };
const STATUS_RANK = { skipped: 0, ok: 1, warning: 2, failed: 3 };
const STATUS_LABEL = { ok: "✅ 正常直连", warning: "⚠️ 播放但发生回退", failed: "❌ 失败", skipped: "⏭️ 跳过" };
const CLIENT_FALLBACK_HINT = {
  stalled: "客户端 demux/remux 在首段窗口内没有产出可播放画面；优先检查该容器/编码的 remux、range 读取与字节对齐。",
  slow: "客户端解码吞吐明显低于实时；检查客户端解码门禁与媒体码率。",
  unviable: "能力门禁在创建客户端引擎前就拒绝了该源；检查 codec string 与位深/分辨率判断。",
  "silent-video": "浏览器播了声音却没有视频帧；检查视频轨解码能力门禁与服务端 remux 输出。",
  "silent-audio": "浏览器声明了音轨但没有解出音频帧；建议加入服务端强制音频转码名单。",
  "direct-play-failed": "直连加载失败后才回退；检查候选 URL、Range 与容器封装。",
};

class UsageError extends Error {}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

function usage() {
  return `用法：LUX_USER=<账号> LUX_PASS=<密码> node scripts/playback-matrix/run-matrix.mjs [options]

环境变量：
  BASE_URL（或 LUX_BASE_URL）  服务地址，默认 ${DEFAULT_BASE_URL}
  LUX_USER                     测试账号（必填）
  LUX_PASS                     密码（必填，只从环境变量读取，绝不写盘）
  BROWSER                      chrome（默认）| webkit | firefox，可逗号分隔多个
  CHROME_PATH                  Chrome 可执行文件，默认 ${DEFAULT_CHROME_PATH}
  MATRIX_HEADED=1              有头运行（真机 GPU 解码/HDR 需要）
  MATRIX_TIMEOUT_MS            单样本最长等待，默认 30000
  MATRIX_RELEASE_MS            about:blank 后的会话释放等待，默认 1500
  MATRIX_OUT                   报告目录，默认 scripts/playback-matrix/reports
  MATRIX_LIBRARY               只测名称包含该子串的媒体库
  MATRIX_FILTER                只测标题匹配该正则的条目
  MATRIX_MAX_ITEMS             最多测试 N 个 source（调试用）
  MATRIX_SCREENSHOTS=0         关闭失败截图
  PLAYWRIGHT_MODULE_DIRS       额外的 node_modules 搜索目录

选项：
  --base-url <url>   --user <name>   --browser <list>
  --library <substr> --filter <regex> --max-items <n>
  --out <dir>        --headed        --screenshots / --no-screenshots
  --help

退出码：0 = 没有 ❌；1 = 存在 ❌；2 = 用法/登录/Playwright 错误。`;
}

function parseArgs(argv) {
  const options = {
    baseUrl: process.env.BASE_URL || process.env.LUX_BASE_URL || DEFAULT_BASE_URL,
    user: process.env.LUX_USER || "",
    browsers: (process.env.BROWSER || "chrome").split(",").map((value) => value.trim()).filter(Boolean),
    library: process.env.MATRIX_LIBRARY || "",
    filter: process.env.MATRIX_FILTER || "",
    maxItems: process.env.MATRIX_MAX_ITEMS ? Number(process.env.MATRIX_MAX_ITEMS) : 0,
    out: process.env.MATRIX_OUT || DEFAULT_REPORT_DIR,
    headed: process.env.MATRIX_HEADED === "1",
    screenshots: process.env.MATRIX_SCREENSHOTS !== "0",
    timeoutMs: Number(process.env.MATRIX_TIMEOUT_MS || 30000),
    releaseMs: Number(process.env.MATRIX_RELEASE_MS || 1500),
    max429Retries: 3,
    help: false,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    const next = () => {
      const value = argv[index + 1];
      if (value === undefined) throw new UsageError(`${arg} 需要一个值`);
      index += 1;
      return value;
    };
    switch (arg) {
      case "--base-url":
        options.baseUrl = next();
        break;
      case "--user":
        options.user = next();
        break;
      case "--browser":
        options.browsers = next().split(",").map((value) => value.trim()).filter(Boolean);
        break;
      case "--library":
        options.library = next();
        break;
      case "--filter":
        options.filter = next();
        break;
      case "--max-items":
        options.maxItems = Number(next());
        break;
      case "--out":
        options.out = next();
        break;
      case "--headed":
        options.headed = true;
        break;
      case "--screenshots":
        options.screenshots = true;
        break;
      case "--no-screenshots":
        options.screenshots = false;
        break;
      case "--help":
      case "-h":
        options.help = true;
        break;
      default:
        throw new UsageError(`未知参数：${arg}`);
    }
  }
  if (options.help) return options;
  if (!options.user) throw new UsageError("缺少 LUX_USER（或 --user）");
  if (!process.env.LUX_PASS) throw new UsageError("缺少 LUX_PASS（只接受环境变量）");
  const invalid = options.browsers.filter((name) => !["chrome", "webkit", "firefox"].includes(name));
  if (invalid.length) throw new UsageError(`不支持的浏览器：${invalid.join(", ")}`);
  if (options.browsers.length === 0) throw new UsageError("BROWSER 不能为空");
  if (!Number.isFinite(options.timeoutMs) || options.timeoutMs < 5000) {
    throw new UsageError("MATRIX_TIMEOUT_MS 必须 >= 5000");
  }
  if (options.filter) {
    try {
      options.filterRegex = new RegExp(options.filter, "i");
    } catch (error) {
      throw new UsageError(`MATRIX_FILTER 不是合法正则：${error.message}`);
    }
  }
  try {
    new URL(options.baseUrl);
  } catch {
    throw new UsageError(`BASE_URL 不是合法 URL：${options.baseUrl}`);
  }
  options.baseUrl = options.baseUrl.replace(/\/+$/, "");
  options.out = path.resolve(options.out);
  return options;
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

function nowIso() {
  return new Date().toISOString();
}

function truncate(value, limit = 400) {
  const text = String(value ?? "").replace(/\s+/g, " ").trim();
  return text.length > limit ? `${text.slice(0, limit)}…` : text;
}

/** Removes query strings/tokens from any text that reaches a report. */
function sanitizeText(value) {
  return String(value ?? "")
    .replace(/(https?:\/\/[^\s"'<>?#]+)\?[^\s"'<>]*/g, "$1?<redacted>")
    .replace(/([?&](?:expires|signature|api_key|PlaySessionId|DeviceId|X-Emby-Token)=)[^&\s"']+/gi, "$1<redacted>")
    .replace(/(luxpb_[A-Za-z0-9_-]+)/g, "luxpb_<redacted>");
}

function slugify(value) {
  return String(value)
    .replace(/[^\p{L}\p{N}]+/gu, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 80) || "sample";
}

function markdownCell(value) {
  return String(value ?? "").replace(/\|/g, "\\|").replace(/\n/g, " ");
}

function codecList(codecs) {
  return codecs.length ? codecs.join(" + ") : "无音轨";
}

// ---------------------------------------------------------------------------
// Playwright loading (same contract as scripts/browser-smoke.mjs)
// ---------------------------------------------------------------------------

function loadPlaywright() {
  const require = createRequire(import.meta.url);
  const moduleDirs = [
    ...(process.env.PLAYWRIGHT_MODULE_DIRS || "").split(path.delimiter),
    ...(process.env.NODE_PATH || "").split(path.delimiter),
    path.join(SCRIPT_DIR, "node_modules"),
    path.join(SCRIPT_DIR, "..", "..", "node_modules"),
    process.cwd(),
  ].filter(Boolean);

  const attempts = [];
  for (const name of ["playwright", "playwright-core"]) {
    try {
      const resolved = require.resolve(name, { paths: moduleDirs });
      return { module: require(resolved), name, resolved };
    } catch (error) {
      attempts.push(`${name}: ${error.code ?? error.message}`);
    }
  }
  throw new UsageError(`找不到 playwright（${attempts.join("；")}）。

请在仓库外的临时目录安装后运行，不要往 web/package.json 添加依赖：
  mkdir -p /tmp/lux-e2e && cd /tmp/lux-e2e
  npm init -y && npm install playwright
  NODE_PATH=/tmp/lux-e2e/node_modules node scripts/playback-matrix/run-matrix.mjs

也可以用 PLAYWRIGHT_MODULE_DIRS=<含 playwright 的 node_modules 目录> 指定已有安装。`);
}

function launcherFor(playwright, name) {
  if (name === "chrome") return playwright.chromium;
  if (name === "webkit") return playwright.webkit;
  return playwright.firefox;
}

async function launchBrowser(playwright, name, options) {
  const launcher = launcherFor(playwright, name);
  const launchOptions = { headless: !options.headed };
  let executablePath = null;
  if (name === "chrome") {
    const candidate = process.env.CHROME_PATH || DEFAULT_CHROME_PATH;
    if (existsSync(candidate)) {
      executablePath = candidate;
      launchOptions.executablePath = candidate;
    }
  }
  const descriptor = {
    name,
    engine: name === "chrome" ? "chromium" : name,
    headed: options.headed,
    executablePath,
    version: null,
    userAgent: null,
    launched: false,
    skippedReason: null,
  };
  try {
    const browser = await launcher.launch(launchOptions);
    descriptor.launched = true;
    descriptor.version = browser.version();
    return { browser, descriptor };
  } catch (error) {
    const message = String(error?.message ?? error);
    if (/Executable doesn't exist|not found|ENOENT/i.test(message)) {
      descriptor.skippedReason = name === "chrome"
        ? `找不到 Chrome：${executablePath ?? "Playwright 自带 Chromium"}。可设置 CHROME_PATH。`
        : `Playwright 未安装 ${name} 浏览器；先运行 npx playwright install ${name}。`;
    } else {
      descriptor.skippedReason = `启动 ${name} 失败：${truncate(sanitizeText(message), 300)}`;
    }
    return { browser: null, descriptor };
  }
}

// ---------------------------------------------------------------------------
// API helpers (same-origin fetch from the logged-in page)
// ---------------------------------------------------------------------------

async function apiGet(page, urlPath) {
  return page.evaluate(async (requestPath) => {
    const response = await fetch(requestPath, {
      credentials: "include",
      headers: { accept: "application/json" },
    });
    const text = await response.text();
    let body = null;
    try {
      body = JSON.parse(text);
    } catch {
      body = null;
    }
    return { status: response.status, body };
  }, urlPath);
}

async function readCsrfToken(page) {
  return page.evaluate(() => {
    const stored = window.localStorage.getItem("lux_csrf_token");
    if (stored) return stored;
    const cookie = document.cookie.split("; ").find((entry) => entry.startsWith("lux_csrf="));
    if (!cookie) return "";
    const raw = cookie.slice("lux_csrf=".length);
    try {
      return decodeURIComponent(raw);
    } catch {
      return raw;
    }
  });
}

/**
 * Clears the item's stored resume position before measuring.
 *
 * The player resumes from the server-side position, so a second matrix run would otherwise start
 * every 5s clip at its end and report a false "no time advanced". Uses the browser context's cookie
 * jar plus the CSRF token captured at login, so it works from `about:blank`.
 */
async function resetItemProgress(context, options, itemId, durationTicks) {
  if (!options.csrfToken) return { status: 0, reason: "missing-csrf" };
  try {
    const response = await context.request.post(
      `${options.baseUrl}/api/v1/items/${encodeURIComponent(itemId)}/progress`,
      {
        headers: { "content-type": "application/json", "x-csrf-token": options.csrfToken },
        data: { positionTicks: 0, durationTicks: durationTicks ?? null, state: "STOPPED" },
      },
    );
    return { status: response.status() };
  } catch (error) {
    return { status: 0, reason: truncate(sanitizeText(error?.message ?? error), 200) };
  }
}

// ---------------------------------------------------------------------------
// Login and enumeration
// ---------------------------------------------------------------------------

async function login(page, options) {
  await page.goto(`${options.baseUrl}/`, { waitUntil: "domcontentloaded", timeout: 30000 });
  if (/\/setup\/?$/.test(page.url())) {
    throw new UsageError("测试实例尚未完成初始化（/setup）。请先完成安装向导再运行矩阵。");
  }
  const usernameField = page.getByLabel("用户名", { exact: true }).first();
  if (await usernameField.count()) {
    await usernameField.fill(options.user);
    await page.getByLabel("密码", { exact: true }).first().fill(process.env.LUX_PASS);
    // The submit label changed across releases ("登录" → "进入影院 ›"), so target the form's submit
    // button instead of one exact copy string.
    const submitButton = page.locator("form button[type=submit]").first();
    const submit = (await submitButton.count())
      ? submitButton.click()
      : page.locator("button").filter({ hasText: /登录|进入影院/ }).first().click();
    const [loginResponse] = await Promise.all([
      page.waitForResponse((response) => response.url().endsWith("/api/v1/auth/login"), { timeout: 20000 }),
      submit,
    ]);
    if (!loginResponse.ok()) {
      throw new UsageError(`登录失败：HTTP ${loginResponse.status()}（检查 LUX_USER / LUX_PASS）。`);
    }
  }
  // Do not depend on a page-specific class name: the logged-in shell changed from `main.content`
  // to `main.lux-main` across releases. `/api/v1/auth/me` is the stable signal that the cookie is
  // valid, and direct `/watch/...` navigation works without rendering the home page first.
  let me = null;
  for (let attempt = 0; attempt < 40; attempt += 1) {
    me = await apiGet(page, "/api/v1/auth/me");
    if (me.status === 200) break;
    await sleep(250);
  }
  if (me?.status !== 200) {
    throw new UsageError(`登录后 /api/v1/auth/me 返回 ${me?.status ?? "无响应"}；账号、密码或实例状态异常。`);
  }
  options.csrfToken = await readCsrfToken(page);
}

async function listLibraries(page, options) {
  const response = await apiGet(page, "/api/v1/libraries");
  if (response.status !== 200) {
    throw new UsageError(`GET /api/v1/libraries 返回 ${response.status}；账号无权访问或服务异常。`);
  }
  let libraries = Array.isArray(response.body?.libraries) ? response.body.libraries : [];
  if (options.library) {
    libraries = libraries.filter((library) => String(library.name ?? "").includes(options.library));
  }
  return libraries;
}

async function listLibraryItems(page, libraryId) {
  const items = [];
  const pageSize = 100; // Server-side upper bound for /api/v1/libraries/{id}/items.
  for (let pageNumber = 1; pageNumber <= 1000; pageNumber += 1) {
    const query = `/api/v1/libraries/${encodeURIComponent(libraryId)}/items?page=${pageNumber}&pageSize=${pageSize}`;
    const response = await apiGet(page, query);
    if (response.status !== 200) {
      throw new UsageError(`读取媒体库 ${libraryId} 条目失败：HTTP ${response.status}`);
    }
    const pageItems = Array.isArray(response.body?.items) ? response.body.items : [];
    items.push(...pageItems);
    if (pageItems.length === 0) break;
    if (typeof response.body?.total === "number" && items.length >= response.body.total) break;
    if (response.body?.hasNext === false) break;
  }
  return items;
}

async function itemDetail(page, itemId) {
  const response = await apiGet(page, `/api/v1/items/${encodeURIComponent(itemId)}`);
  return response.status === 200 ? response.body : null;
}

const KNOWN_VIDEO_CODECS = ["h264", "hevc", "vp8", "vp9", "av1", "mpeg2video", "mpeg4", "vc1", "mjpeg", "prores"];

function codecsFromTitle(title) {
  const lower = String(title ?? "").toLowerCase();
  const video = KNOWN_VIDEO_CODECS.find((codec) => lower.includes(codec.replace("mpeg2video", "mpeg2")))
    ?? (lower.includes("mpeg2") ? "mpeg2video" : "unknown");
  const audioTokens = ["aac", "mp3", "mp2", "ac3", "eac3", "dts", "truehd", "flac", "opus", "vorbis", "pcm", "alac", "wma"];
  const audio = audioTokens.filter((token) => lower.includes(token)).map((token) => (
    token === "wma" ? "wmav2" : token === "pcm" ? "pcm" : token
  ));
  return { video, audio };
}

function describeSource(item, source, sourceIndex = 0, sourceCount = 1) {
  const streams = Array.isArray(source.streams) ? source.streams : [];
  const videoStreams = streams.filter((stream) => String(stream.type ?? "").toUpperCase() === "VIDEO");
  const audioStreams = streams.filter((stream) => String(stream.type ?? "").toUpperCase() === "AUDIO");
  const subtitleStreams = streams.filter((stream) => String(stream.type ?? "").toUpperCase() === "SUBTITLE");
  const baseTitle = item.title || item.name || "(untitled)";
  const fallback = codecsFromTitle(`${baseTitle} ${source.qualityLabel ?? ""}`);
  const videoCodec = videoStreams[0]?.codec || fallback.video;
  const audioCodecs = audioStreams.length
    ? audioStreams.map((stream) => stream.codec || "unknown")
    : (fallback.audio.length ? fallback.audio : []);
  // The scanner strips codec tokens from titles and merges files that clean up to the same name, so
  // several samples can share one item. Keep the version label (HDR/4K/…) or, for unlabeled
  // versions, the ordinal plus the actual codecs so two rows never look identical.
  const variant = source.qualityLabel || source.editionName
    || (sourceCount > 1
      ? `版本 ${sourceIndex + 1}（${videoCodec}/${audioCodecs.length ? audioCodecs.join("+") : "无音轨"}）`
      : "");
  const title = variant ? `${baseTitle} · ${variant}` : baseTitle;
  return {
    itemId: item.id,
    libraryId: item.libraryId ?? null,
    title,
    sourceId: source.id,
    container: (source.container || path.extname(source.externalUrl ?? "") || "unknown").replace(/^\./, "").toLowerCase(),
    videoCodec,
    audioCodecs,
    subtitleCodecs: subtitleStreams.map((stream) => stream.codec || "unknown"),
    hasAudio: audioStreams.length > 0 || fallback.audio.length > 0,
    durationTicks: source.durationTicks ?? null,
  };
}

async function collectSamples(page, options) {
  const libraries = await listLibraries(page, options);
  if (libraries.length === 0) {
    throw new UsageError("没有可见媒体库（或 --library 过滤后为空）。请确认测试账号的媒体库权限。");
  }
  const samples = [];
  const skippedItems = [];
  for (const library of libraries) {
    const items = await listLibraryItems(page, library.id);
    for (const item of items) {
      const itemType = String(item.itemType ?? "").toUpperCase();
      const itemTitle = item.title || item.name || "(untitled)";
      if (options.filterRegex && !options.filterRegex.test(itemTitle)) continue;
      let sources = Array.isArray(item.mediaSources) ? item.mediaSources : [];
      if (sources.length === 0) {
        const detail = await itemDetail(page, item.id);
        sources = Array.isArray(detail?.mediaSources) ? detail.mediaSources : [];
      }
      if (sources.length === 0) {
        if (itemType === "MOVIE" || itemType === "EPISODE") {
          skippedItems.push({ title: itemTitle, reason: "条目没有媒体源（可能尚未探测完成）" });
        }
        continue;
      }
      for (const [sourceIndex, source] of sources.entries()) {
        if (!source?.id) continue;
        samples.push(describeSource(item, source, sourceIndex, sources.length));
      }
    }
  }
  samples.sort((left, right) => (
    left.title.localeCompare(right.title, "zh-Hans-CN") || left.sourceId.localeCompare(right.sourceId)
  ));
  if (options.maxItems > 0) samples.length = Math.min(samples.length, options.maxItems);
  return { samples, skippedItems, libraries };
}

// ---------------------------------------------------------------------------
// Playback measurement
// ---------------------------------------------------------------------------

async function readPlaybackState(page) {
  return page.evaluate(() => {
    const video = document.querySelector("video");
    const alertElement = document.querySelector("[role=alert]");
    const alertText = alertElement ? (alertElement.textContent || "").trim() : "";
    if (!video) {
      return {
        exists: false,
        hasFrame: false,
        currentTime: 0,
        duration: 0,
        readyState: 0,
        paused: true,
        ended: false,
        videoWidth: 0,
        videoHeight: 0,
        audioDecodedBytes: null,
        mediaError: null,
        blockingAlert: alertText.length > 0,
        alertText,
        muted: false,
        currentSrc: false,
      };
    }
    const decoded = typeof video.webkitAudioDecodedByteCount === "number"
      ? video.webkitAudioDecodedByteCount
      : null;
    return {
      exists: true,
      hasFrame: video.videoWidth > 0 && video.videoHeight > 0,
      currentTime: Number.isFinite(video.currentTime) ? video.currentTime : 0,
      duration: Number.isFinite(video.duration) ? video.duration : 0,
      readyState: video.readyState,
      paused: video.paused,
      ended: video.ended,
      videoWidth: video.videoWidth,
      videoHeight: video.videoHeight,
      audioDecodedBytes: decoded,
      mediaError: video.error ? { code: video.error.code, message: video.error.message } : null,
      blockingAlert: alertText.length > 0,
      alertText,
      muted: video.muted,
      currentSrc: Boolean(video.currentSrc || video.getAttribute("src")),
    };
  });
}

async function tryPlay(page) {
  return page.evaluate(async () => {
    const video = document.querySelector("video");
    if (!video) return { result: "no-video" };
    if (!video.paused && !video.ended) return { result: "already-playing" };
    if (!video.currentSrc && !video.getAttribute("src") && video.readyState === 0) {
      return { result: "no-source" };
    }
    try {
      await video.play();
      return { result: "played", muted: video.muted };
    } catch (error) {
      try {
        video.muted = true;
        await video.play();
        return { result: "played-muted", muted: true, error: error?.name ?? "unknown" };
      } catch (secondError) {
        return { result: "blocked", error: `${error?.name ?? "unknown"} / ${secondError?.name ?? "unknown"}` };
      }
    }
  });
}

function parseFallback(consoleMessages) {
  const fallbacks = [];
  for (const message of consoleMessages) {
    if (!message.text.includes("[Lux 播放器]")) continue;
    const reasonMatch = message.text.match(/回退为服务端转码：([a-z-]+)/);
    const detailMatch = message.text.match(/（([^）]*)）/);
    fallbacks.push({
      reason: reasonMatch?.[1] ?? "direct-play-failed",
      detail: detailMatch?.[1] ?? null,
      text: message.text,
    });
  }
  return fallbacks;
}

function planLabel(plan) {
  if (!plan) return "未捕获";
  if (plan.type === "DIRECT") return "DIRECT（直连）";
  if (plan.type === "UNSUPPORTED") return `UNSUPPORTED（${plan.reason ?? "无原因码"}）`;
  if (plan.type === "SERVER_HLS") {
    const tier = plan.tier;
    if (tier === 1) return "REMUX（SERVER_HLS tier 1）";
    if (tier === 2) return "SERVER_HLS tier 2（视频 copy + 音频转码）";
    if (tier === 3) return "SERVER_HLS tier 3（硬件转码）";
    if (tier === 4) return "SERVER_HLS tier 4（软件转码）";
    return `SERVER_HLS tier ${tier ?? "?"}`;
  }
  return String(plan.type);
}

async function measureSample(page, context, sample, options) {
  const collectors = {
    console: [],
    consoleErrors: [],
    pageErrors: [],
    failedRequests: [],
    playbackResponses: [],
    // Increments on every successful playback session response; the player swaps the element and
    // restarts playback after a client fallback, so measurement must start over for the new attempt.
    playbackEpoch: 0,
    rateLimited: false,
  };
  const onConsole = (message) => {
    const entry = { type: message.type(), text: sanitizeText(message.text()) };
    collectors.console.push(entry);
    // Browsers emit one generic message per missing static asset (favicon, poster) without the URL;
    // playback API failures are captured with their HTTP status in `failedRequests` instead.
    const genericResource = /^Failed to load resource: the server responded with a status of (?:401|404)/.test(entry.text);
    if (entry.type === "error" && !genericResource) collectors.consoleErrors.push(entry.text);
  };
  const onPageError = (error) => {
    collectors.pageErrors.push(sanitizeText(error?.message ?? error));
  };
  const onRequestFailed = (request) => {
    try {
      const url = new URL(request.url());
      if (!/\/api\/v1\/playback\/|\/stream|\/direct|\/hls\//.test(url.pathname)) return;
      // The player aborts the previous session and in-flight HLS segments on fallback/teardown;
      // that is expected and must not look like a playback failure.
      if (/ERR_ABORTED/i.test(request.failure()?.errorText ?? "")) return;
      collectors.failedRequests.push({
        path: url.pathname,
        error: sanitizeText(request.failure()?.errorText ?? "unknown"),
      });
    } catch {
      // Ignore non-URL request targets (data:, blob:).
    }
  };
  const onResponse = (response) => {
    try {
      const url = new URL(response.url());
      if (!url.pathname.startsWith("/api/v1/playback/")) return;
      if (response.status() === 429) collectors.rateLimited = true;
      if (response.status() >= 400 && /^\/api\/v1\/playback\//.test(url.pathname)) {
        collectors.failedRequests.push({ path: url.pathname, error: `HTTP ${response.status()}` });
      }
      if (response.request().method() !== "POST") return;
      if (!/\/api\/v1\/playback\/(sessions|bootstrap)$/.test(url.pathname)) return;
      if (response.status() === 200) collectors.playbackEpoch += 1;
      const entry = {
        endpoint: url.pathname.endsWith("bootstrap") ? "bootstrap" : "sessions",
        status: response.status(),
      };
      pendingResponses += 1;
      response.json().then((body) => {
        const session = body?.session ?? body;
        if (session?.plan?.type) {
          entry.plan = { type: session.plan.type };
          if (session.plan.reason) entry.plan.reason = session.plan.reason;
          entry.tier = session.tier ?? session.plan.tier ?? null;
        }
        collectors.playbackResponses.push(entry);
      }).catch(() => {
        collectors.playbackResponses.push(entry);
      }).finally(() => {
        pendingResponses -= 1;
      });
    } catch {
      // Ignore malformed URLs.
    }
  };
  page.on("console", onConsole);
  page.on("pageerror", onPageError);
  page.on("requestfailed", onRequestFailed);
  page.on("response", onResponse);

  let pendingResponses = 0;
  const result = {
    status: "failed",
    plan: null,
    tier: null,
    planText: "未捕获",
    fallbacks: [],
    clientFallback: false,
    video: { width: 0, height: 0, frames: false, currentTimeAdvance: 0, readyState: 0 },
    audio: { decoded: "unknown", decodedBytes: null },
    timing: { navigationMs: 0, firstFrameMs: null, measuredMs: 0 },
    blockingAlert: null,
    mediaError: null,
    playError: null,
    progressReset: null,
    attempts: 1,
    skippedReason: null,
    consoleErrors: [],
    consolePlayback: [],
    pageErrors: [],
    failedRequests: [],
    rateLimited: false,
    sample,
  };

  try {
    if (options.resetProgress) {
      result.progressReset = await resetItemProgress(context, options, sample.itemId, sample.durationTicks);
    }
    const navigationStarted = Date.now();
    const watchUrl = `${options.baseUrl}/watch/${encodeURIComponent(sample.itemId)}?sourceId=${encodeURIComponent(sample.sourceId)}`;
    await page.goto(watchUrl, { waitUntil: "domcontentloaded", timeout: Math.min(25000, options.timeoutMs) });
    result.timing.navigationMs = Date.now() - navigationStarted;

    let nudged = false;
    let seeded = false;
    let reseeked = false;
    let initialCurrent = null;
    let maxCurrent = 0;
    let firstFrameMs = null;
    let blockingAlert = null;
    let silentAudioGraceUntil = null;
    let lastState = null;
    let seenEpoch = 0;
    const deadline = Date.now() + options.timeoutMs;
    while (Date.now() < deadline) {
      const state = await readPlaybackState(page);
      if (collectors.playbackEpoch !== seenEpoch || (lastState?.exists && !state.exists)) {
        // A new session (client fallback) or a remounted element means the measured advance belongs
        // to the previous attempt; restart the counters and give the new playback the full budget.
        seenEpoch = collectors.playbackEpoch;
        seeded = false;
        reseeked = false;
        initialCurrent = null;
        maxCurrent = 0;
        firstFrameMs = null;
        silentAudioGraceUntil = null;
      }
      lastState = state;
      // A previous run (or another version of the same item) leaves a resume position behind. Start
      // from zero once the element has metadata, so the measured advance is real playback rather
      // than a seek straight to the end of a 5s sample.
      if (!seeded && state.exists && state.readyState >= 1) {
        await page.evaluate(() => {
          const video = document.querySelector("video");
          if (!video) return;
          try {
            video.currentTime = 0;
          } catch {
            // The element may not be seekable yet; the next poll retries.
          }
        });
        seeded = true;
        initialCurrent = 0;
        maxCurrent = 0;
      }
      if (seeded && !reseeked && state.currentTime > 2 && maxCurrent < 1) {
        // The player restored the resume position after our seek; take it back once.
        await page.evaluate(() => {
          const video = document.querySelector("video");
          if (!video) return;
          try {
            video.currentTime = 0;
          } catch {
            // Ignore; the deadline still bounds the sample.
          }
        });
        reseeked = true;
        initialCurrent = 0;
        maxCurrent = 0;
        await sleep(150);
        continue;
      }
      if (state.exists && initialCurrent === null) initialCurrent = state.currentTime;
      if (state.currentTime > maxCurrent) maxCurrent = state.currentTime;
      if (state.hasFrame && firstFrameMs === null) firstFrameMs = Date.now() - navigationStarted;
      if (state.blockingAlert) {
        blockingAlert = state.alertText;
      }
      if (!nudged && state.exists && state.paused) {
        const play = await tryPlay(page);
        if (play.result === "played" || play.result === "played-muted") {
          nudged = true;
          result.playError = play.result === "played-muted" ? { muted: true, error: play.error ?? null } : null;
        } else if (play.result === "blocked" || play.result === "no-source") {
          // Retry once after a real click; a click is a user gesture for autoplay policy.
          if (play.result === "blocked") {
            try {
              await page.mouse.click(640, 360);
            } catch {
              // A click can fail while the page is still navigating.
            }
            const retry = await tryPlay(page);
            if (retry.result === "played" || retry.result === "played-muted") {
              nudged = true;
              result.playError = retry.result === "played-muted" ? { muted: true, error: retry.error ?? null } : null;
            } else {
              result.playError = { blocked: retry.error ?? play.error ?? "unknown" };
            }
          }
        }
        nudged = true;
      }
      const advance = maxCurrent - (initialCurrent ?? 0);
      if (blockingAlert) break;
      if (advance >= options.advanceSeconds) {
        // A slow-starting audio track can legitimately report 0 decoded bytes at 1.5s. Give
        // Chromium up to two extra seconds before calling it silent (other engines do not expose
        // the counter and break immediately).
        const audioStillSilent = sample.hasAudio
          && typeof state.audioDecodedBytes === "number"
          && state.audioDecodedBytes === 0;
        if (audioStillSilent && silentAudioGraceUntil === null) {
          silentAudioGraceUntil = Date.now() + 2000;
        }
        if (!audioStillSilent || Date.now() >= silentAudioGraceUntil) break;
      }
      if (state.ended && state.hasFrame) break;
      await sleep(250);
    }

    const finalState = lastState ?? (await readPlaybackState(page));
    const advance = Math.max(0, maxCurrent - (initialCurrent ?? 0));
    result.video = {
      width: finalState.videoWidth,
      height: finalState.videoHeight,
      frames: Boolean(finalState.hasFrame),
      currentTimeAdvance: Number(advance.toFixed(2)),
      resumedAt: initialCurrent && initialCurrent > 0.5 ? Number(initialCurrent.toFixed(2)) : 0,
      readyState: finalState.readyState,
      ended: Boolean(finalState.ended),
      duration: Number.isFinite(finalState.duration) ? Number(finalState.duration.toFixed(2)) : 0,
    };
    const decodedBytes = finalState.audioDecodedBytes;
    result.audio = {
      decodedBytes,
      decoded: !sample.hasAudio
        ? "n/a"
        : typeof decodedBytes !== "number"
          ? "unknown"
          : decodedBytes > 0 ? "decoded" : "silent",
    };
    result.timing.firstFrameMs = firstFrameMs;
    result.blockingAlert = blockingAlert ? truncate(sanitizeText(blockingAlert), 300) : null;
    result.mediaError = finalState.mediaError ? {
      code: finalState.mediaError.code,
      message: truncate(sanitizeText(finalState.mediaError.message), 200),
    } : null;
    result.playError = result.playError ?? null;

    // Wait for the response body parsers (bootstrap/session JSON) before snapshotting the plan.
    const responseDeadline = Date.now() + 1500;
    while (pendingResponses > 0 && Date.now() < responseDeadline) await sleep(50);
    await sleep(100);
    result.rateLimited = collectors.rateLimited;
    result.consoleErrors = collectors.consoleErrors.slice(0, 10);
    result.consolePlayback = collectors.console
      .filter((entry) => entry.text.includes("[Lux 播放器]"))
      .map((entry) => entry.text);
    result.pageErrors = collectors.pageErrors.slice(0, 10);
    result.failedRequests = collectors.failedRequests.slice(0, 10);
    result.fallbacks = parseFallback(collectors.console).map((fallback) => ({
      reason: fallback.reason,
      detail: truncate(fallback.detail, 200),
      text: truncate(sanitizeText(fallback.text), 300),
    }));
    result.clientFallback = result.fallbacks.length > 0;

    const successful = collectors.playbackResponses.filter((entry) => entry.status === 200 && entry.plan);
    const finalSession = successful.length ? successful[successful.length - 1] : null;
    if (finalSession) {
      result.plan = finalSession.plan;
      result.tier = finalSession.tier;
      result.planText = planLabel({ ...finalSession.plan, tier: finalSession.tier });
    } else if (collectors.playbackResponses.length) {
      const last = collectors.playbackResponses[collectors.playbackResponses.length - 1];
      result.planText = `播放会话 HTTP ${last.status}（未获得计划）`;
    }
    result.playbackResponses = collectors.playbackResponses.map((entry) => ({
      endpoint: entry.endpoint,
      status: entry.status,
      plan: entry.plan?.type ?? null,
      reason: entry.plan?.reason ?? null,
      tier: entry.tier ?? null,
    }));

    result.status = classifyResult(result, sample);
    result.timing.measuredMs = Date.now() - navigationStarted;
    return result;
  } finally {
    page.off("console", onConsole);
    page.off("pageerror", onPageError);
    page.off("requestfailed", onRequestFailed);
    page.off("response", onResponse);
  }
}

function errorMeasurement(sample, error) {
  return {
    status: "failed",
    plan: null,
    tier: null,
    planText: "测量异常",
    fallbacks: [],
    clientFallback: false,
    video: { width: 0, height: 0, frames: false, currentTimeAdvance: 0, readyState: 0 },
    audio: { decoded: sample.hasAudio ? "unknown" : "n/a", decodedBytes: null },
    timing: { navigationMs: 0, firstFrameMs: null, measuredMs: 0 },
    blockingAlert: null,
    mediaError: null,
    playError: null,
    progressReset: null,
    attempts: 1,
    skippedReason: null,
    consoleErrors: [],
    consolePlayback: [],
    pageErrors: [],
    failedRequests: [],
    rateLimited: false,
    playbackResponses: [],
    measureError: truncate(sanitizeText(error?.message ?? error), 300),
    sample,
  };
}

function classifyResult(result, sample) {
  if (result.plan?.type === "UNSUPPORTED") return "failed";
  if (result.blockingAlert) return "failed";
  if (result.mediaError) return "failed";
  const played = result.video.frames && (result.video.currentTimeAdvance >= 1.5 || (result.video.ended && result.video.currentTimeAdvance > 0));
  if (!played) return "failed";
  const fallback = result.plan?.type !== "DIRECT" || result.clientFallback;
  const silentAudio = sample.hasAudio && result.audio.decoded === "silent";
  if (fallback || silentAudio) return "warning";
  return "ok";
}

// ---------------------------------------------------------------------------
// Reporting
// ---------------------------------------------------------------------------

function compareCodecs(left, right) {
  return left.localeCompare(right, "en");
}

function buildMatrix(browserName, results) {
  const tested = results.filter((result) => result.browser === browserName);
  const videoLabels = [...new Set(tested.map((result) => result.sample.videoCodec))].sort(compareCodecs);
  const audioLabels = [...new Set(tested.map((result) => codecList(result.sample.audioCodecs)))].sort(compareCodecs);
  const cells = new Map();
  for (const result of tested) {
    const video = result.sample.videoCodec;
    const audio = codecList(result.sample.audioCodecs);
    const key = `${video}\u0000${audio}`;
    const cell = cells.get(key) ?? { video, audio, statuses: { ok: 0, warning: 0, failed: 0, skipped: 0 }, samples: [] };
    cell.statuses[result.status] += 1;
    cell.samples.push(result.sample.title);
    cells.set(key, cell);
  }
  for (const cell of cells.values()) {
    const statuses = Object.keys(cell.statuses).filter((key) => cell.statuses[key] > 0);
    cell.status = statuses.reduce((current, status) => (
      current === null || STATUS_RANK[status] > STATUS_RANK[current] ? status : current
    ), null);
    cell.total = statuses.reduce((sum, status) => sum + cell.statuses[status], 0);
  }
  return { videoLabels, audioLabels, cells: [...cells.values()] };
}

function matrixMarkdown(matrix) {
  const header = ["视频编码 \\ 音频编码", ...matrix.audioLabels];
  const lines = [
    `| ${header.map(markdownCell).join(" | ")} |`,
    `| ${header.map(() => "---").join(" | ")} |`,
  ];
  for (const video of matrix.videoLabels) {
    const row = [video];
    for (const audio of matrix.audioLabels) {
      const cell = matrix.cells.find((entry) => entry.video === video && entry.audio === audio);
      if (!cell) {
        row.push("—");
        continue;
      }
      const suffix = cell.total > 1 ? `×${cell.total}` : "";
      row.push(`${STATUS_SYMBOL[cell.status]}${suffix}`);
    }
    lines.push(`| ${row.map(markdownCell).join(" | ")} |`);
  }
  return lines.join("\n");
}

function resultDetail(result) {
  const sample = result.sample;
  const lines = [];
  lines.push(`- **${sample.title}** · ${result.browserLabel} · 容器 \`${sample.container}\``);
  lines.push(`  - 编码：视频 \`${sample.videoCodec}\` / 音频 ${sample.audioCodecs.length ? sample.audioCodecs.map((codec) => `\`${codec}\``).join(" + ") : "无音轨"}${sample.subtitleCodecs.length ? ` / 字幕 ${sample.subtitleCodecs.map((codec) => `\`${codec}\``).join(" + ")}` : ""}`);
  lines.push(`  - 服务端计划：${result.planText}`);
  if (result.fallbacks.length) {
    lines.push(`  - 客户端回退：${result.fallbacks.map((fallback) => `\`${fallback.reason}\`${fallback.detail ? `（${fallback.detail}）` : ""}`).join("；")}`);
  }
  lines.push(`  - 画面：${result.video.frames ? `${result.video.width}×${result.video.height}` : "无画面"}，currentTime 前进 ${result.video.currentTimeAdvance}s，readyState=${result.video.readyState}`);
  lines.push(`  - 音频：${result.audio.decoded === "decoded" ? `已解码（${result.audio.decodedBytes} 字节）` : result.audio.decoded === "silent" ? "**静默（0 字节）**" : result.audio.decoded === "n/a" ? "无音轨" : result.engine === "chromium" ? "未知（未读到解码计数器）" : "未知（非 Chromium 引擎）"}`);
  lines.push(`  - 首帧：${result.timing.firstFrameMs === null ? "未出现" : `${(result.timing.firstFrameMs / 1000).toFixed(2)}s`}（最终会话）；导航 ${result.timing.navigationMs}ms；尝试 ${result.attempts} 次`);
  if (result.blockingAlert) lines.push(`  - 阻塞提示（role=alert）：${result.blockingAlert}`);
  if (result.mediaError) lines.push(`  - 媒体错误：code=${result.mediaError.code} ${result.mediaError.message ?? ""}`);
  if (result.playError?.blocked) lines.push(`  - 自动播放失败：${result.playError.blocked}`);
  if (result.progressReset && result.progressReset.status !== 200) {
    lines.push(`  - 进度重置：HTTP ${result.progressReset.status}${result.progressReset.reason ? `（${result.progressReset.reason}）` : ""}`);
  }
  if (result.consoleErrors.length) lines.push(`  - 控制台错误：${result.consoleErrors.map((text) => `\`${truncate(text, 160)}\``).join("；")}`);
  if (result.pageErrors.length) lines.push(`  - 页面异常：${result.pageErrors.map((text) => `\`${truncate(text, 160)}\``).join("；")}`);
  if (result.failedRequests.length) lines.push(`  - 失败请求：${result.failedRequests.map((request) => `\`${request.path}\` (${request.error})`).join("；")}`);
  return lines.join("\n");
}

function buildSuspiciousRules(results) {
  const rules = [];
  const played = results.filter((result) => result.status !== "skipped");
  const chromium = played.filter((result) => result.engine === "chromium");

  const audioCodecs = [...new Set(played.flatMap((result) => result.sample.audioCodecs))];
  for (const codec of audioCodecs) {
    const chromiumSamples = chromium.filter((result) => result.sample.audioCodecs.includes(codec));
    const silent = chromiumSamples.filter((result) => result.audio.decoded === "silent");
    if (silent.length === 0) continue;
    const allSilent = silent.length === chromiumSamples.length;
    rules.push({
      severity: allSilent ? "high" : "medium",
      axis: "audio",
      codec,
      code: allSilent ? "audio-silent-all-chromium" : "audio-silent-some-chromium",
      browsers: [...new Set(silent.map((result) => result.browser))],
      evidence: `${silent.length}/${chromiumSamples.length} 个 Chromium 样本视频在前进但 webkitAudioDecodedByteCount=0`,
      samples: silent.map((result) => result.sample.title),
      suggestion: `建议将 ${codec} 加入服务端强制音频转码名单（音频转码档位），或对声明支持 ${codec} 的浏览器补充解码验证后再放行直连。`,
    });
  }

  const videoCodecs = [...new Set(played.map((result) => result.sample.videoCodec))];
  for (const codec of videoCodecs) {
    const samples = played.filter((result) => result.sample.videoCodec === codec);
    const failed = samples.filter((result) => result.status === "failed");
    if (failed.length === samples.length && samples.length > 0) {
      rules.push({
        severity: "high",
        axis: "video",
        codec,
        code: "video-unplayable",
        browsers: [...new Set(samples.map((result) => result.browser))],
        evidence: `${samples.length}/${samples.length} 个样本失败`,
        samples: samples.map((result) => result.sample.title),
        suggestion: `\`${codec}\` 在所有测试样本上都失败；检查客户端能力门禁、服务端 remux 输出与容器封装，并在修复前默认走服务端转码。`,
      });
      continue;
    }
    const alwaysFallback = samples.filter((result) => result.plan?.type !== "DIRECT" || result.clientFallback);
    if (alwaysFallback.length === samples.length && samples.length > 0) {
      rules.push({
        severity: "medium",
        axis: "video",
        codec,
        code: "video-never-direct",
        browsers: [...new Set(samples.map((result) => result.browser))],
        evidence: `${samples.length}/${samples.length} 个样本都发生了回退（服务端计划或客户端回退）`,
        samples: samples.map((result) => result.sample.title),
        suggestion: `\`${codec}\` 从未成功直连；如果它在目标浏览器上本应可直连，请检查 remux/容器路径；否则将其视为需要服务端处理的编码并补齐档位。`,
      });
    }
  }

  const containers = [...new Set(played.map((result) => result.sample.container))];
  for (const container of containers) {
    const samples = played.filter((result) => result.sample.container === container);
    const fallback = samples.filter((result) => result.clientFallback);
    if (fallback.length > 0) {
      rules.push({
        severity: fallback.length === samples.length ? "high" : "medium",
        axis: "container",
        codec: container,
        code: "client-fallback-container",
        browsers: [...new Set(fallback.map((result) => result.browser))],
        evidence: `${fallback.length}/${samples.length} 个 \`${container}\` 样本触发客户端回退`,
        samples: fallback.map((result) => result.sample.title),
        suggestion: `\`.${container}\` 的客户端路径（demux/remux/range 读取）疑似不稳定；优先排查字节对齐与首段窗口超时。`,
      });
    }
  }

  const fallbackReasons = new Map();
  for (const result of played) {
    for (const fallback of result.fallbacks) {
      const entry = fallbackReasons.get(fallback.reason) ?? { count: 0, samples: [] };
      entry.count += 1;
      entry.samples.push(result.sample.title);
      fallbackReasons.set(fallback.reason, entry);
    }
  }
  for (const [reason, entry] of fallbackReasons) {
    rules.push({
      severity: "medium",
      axis: "fallback",
      codec: reason,
      code: `client-fallback-${reason}`,
      browsers: [...new Set(played.filter((result) => result.fallbacks.some((fallback) => fallback.reason === reason)).map((result) => result.browser))],
      evidence: `${entry.count} 次客户端回退原因为 \`${reason}\``,
      samples: [...new Set(entry.samples)],
      suggestion: CLIENT_FALLBACK_HINT[reason] ?? "检查该回退原因对应的客户端解码路径。",
    });
  }

  const failedPairs = new Map();
  for (const result of played.filter((entry) => entry.status === "failed")) {
    const key = `${result.sample.videoCodec} × ${codecList(result.sample.audioCodecs)}`;
    const entry = failedPairs.get(key) ?? { count: 0, samples: [], browsers: new Set() };
    entry.count += 1;
    entry.samples.push(result.sample.title);
    entry.browsers.add(result.browser);
    failedPairs.set(key, entry);
  }
  for (const [pair, entry] of failedPairs) {
    rules.push({
      severity: "high",
      axis: "pair",
      codec: pair,
      code: "codec-pair-failed",
      browsers: [...entry.browsers],
      evidence: `${entry.count} 个样本在 ${pair} 组合上失败`,
      samples: [...new Set(entry.samples)],
      suggestion: `复核 ${pair} 的服务端计划与浏览器能力判断；确认这是客户端问题还是样本本身不可解码。`,
    });
  }

  return rules.sort((left, right) => (
    (left.severity === right.severity ? 0 : left.severity === "high" ? -1 : 1)
    || left.code.localeCompare(right.code)
  ));
}

function reportMarkdown(report) {
  const lines = [];
  lines.push("# Web 播放兼容性矩阵报告");
  lines.push("");
  lines.push(`- 生成时间：${report.generatedAt}`);
  lines.push(`- 服务地址：${report.baseUrl}`);
  lines.push(`- 浏览器：${report.browsers.map((browser) => `${browser.label}${browser.launched ? "" : `（跳过：${browser.skippedReason}）`}`).join("；")}`);
  lines.push(`- 样本：${report.totals.samples} 个 source，完成 ${report.totals.measured} 次测量（多浏览器会重复计），跳过 ${report.totals.skipped} 次`);
  lines.push(`- 结果：✅ ${report.totals.ok} · ⚠️ ${report.totals.warning} · ❌ ${report.totals.failed} · ⏭️ ${report.totals.skipped}`);
  lines.push(`- 机器可读结果：\`report.json\`（同目录）`);
  lines.push("");
  lines.push("> 符号：✅ 直连且画面/音频正常 · ⚠️ 播放成功但发生回退（服务端转码/重封装，或客户端回退，或音频静默）· ❌ 失败（无画面、未前进、阻塞提示或媒体错误）· ⏭️ 跳过");
  lines.push("> 音频解码只在 Chromium 引擎可测（`webkitAudioDecodedByteCount`）；Firefox/WebKit 记为“未知”。");
  if (report.headless) {
    lines.push("> 本次为无头运行：无头 Chrome 的 HEVC/HDR 解码与真机窗口可能不同，涉及硬件解码的结论请用 `MATRIX_HEADED=1` 复验。");
  }
  lines.push("");

  for (const browser of report.browsers) {
    if (!browser.launched) continue;
    const matrix = report.matrix[browser.name];
    if (!matrix || matrix.videoLabels.length === 0) {
      lines.push(`## ${browser.label}`);
      lines.push("");
      lines.push("没有测量到任何样本。");
      lines.push("");
      continue;
    }
    lines.push(`## ${browser.label} 矩阵`);
    lines.push("");
    lines.push(matrixMarkdown(matrix));
    lines.push("");
  }

  const failures = report.results.filter((result) => result.status === "failed");
  lines.push("## ❌ 失败详情");
  lines.push("");
  if (failures.length === 0) {
    lines.push("没有失败样本。");
  } else {
    for (const result of failures) {
      lines.push(resultDetail(result));
      lines.push("");
    }
  }
  lines.push("");

  const warnings = report.results.filter((result) => result.status === "warning");
  lines.push("## ⚠️ 回退/静默详情");
  lines.push("");
  if (warnings.length === 0) {
    lines.push("没有需要关注的回退样本。");
  } else {
    for (const result of warnings) {
      lines.push(resultDetail(result));
      lines.push("");
    }
  }
  lines.push("");

  const skipped = report.results.filter((result) => result.status === "skipped");
  const skippedItems = report.skippedItems ?? [];
  if (skipped.length || skippedItems.length || report.browsers.some((browser) => !browser.launched)) {
    lines.push("## ⏭️ 跳过项");
    lines.push("");
    for (const browser of report.browsers.filter((entry) => !entry.launched)) {
      lines.push(`- 浏览器 ${browser.name}：${browser.skippedReason}`);
    }
    for (const result of skipped) {
      lines.push(`- ${result.sample.title}（${result.browserLabel}）：${result.skippedReason}`);
    }
    for (const item of skippedItems) {
      lines.push(`- 条目“${item.title}”：${item.reason}`);
    }
    lines.push("");
  }

  lines.push("## 可疑规则汇总建议");
  lines.push("");
  if (report.suspiciousRules.length === 0) {
    lines.push("没有从本次矩阵中提炼出可疑规则。");
  } else {
    report.suspiciousRules.forEach((rule, index) => {
      lines.push(`### ${index + 1}. ${rule.axis === "audio" ? "音频" : rule.axis === "video" ? "视频" : rule.axis === "container" ? "容器" : rule.axis === "pair" ? "编码组合" : "回退原因"}：\`${rule.codec}\`（${rule.severity === "high" ? "高" : "中"}）`);
      lines.push("");
      lines.push(`- 证据：${rule.evidence}（浏览器：${rule.browsers.join("、") || "未知"}）`);
      lines.push(`- 样本：${rule.samples.slice(0, 8).join("、")}${rule.samples.length > 8 ? ` 等 ${rule.samples.length} 个` : ""}`);
      lines.push(`- 建议：${rule.suggestion}`);
      lines.push("");
    });
  }
  lines.push("");
  lines.push("## 附注");
  lines.push("");
  lines.push("- 本工具不覆盖 `.strm` 源，也不会并发执行；单次运行始终串行播放并在每个样本后回到 `about:blank` 释放会话。");
  lines.push("- 样本生成器无法产出的维度（VC-1、PGS、HE-AAC、DTS-HD MA、AC-3/E-AC-3 7.1、Vorbis 5.1）记录在 `generate-samples.mjs` 的 `manifest.json` 的 `skipped` 中。");
  lines.push("- 若某个样本需要多次尝试，`尝试 N 次` 表示 429 并发上限退避重试；仍失败则整条样本记为跳过。");
  return lines.join("\n");
}

async function writeReports(options, report) {
  await mkdir(options.out, { recursive: true });
  const jsonPath = path.join(options.out, "report.json");
  const markdownPath = path.join(options.out, "report.md");
  await writeFile(jsonPath, `${JSON.stringify(report, null, 2)}\n`, "utf8");
  await writeFile(markdownPath, `${reportMarkdown(report)}\n`, "utf8");
  return { jsonPath, markdownPath };
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

async function run() {
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

  const playwrightInfo = loadPlaywright();
  const playwright = playwrightInfo.module;
  console.error(`Playwright：${playwrightInfo.name}（${playwrightInfo.resolved}）`);
  options.advanceSeconds = 1.5;
  options.resetProgress = process.env.MATRIX_RESET_PROGRESS !== "0";
  options.timeoutMs = Math.max(5000, options.timeoutMs);

  const startedAt = nowIso();
  const results = [];
  const browsers = [];
  const skippedItems = [];
  let samples = [];
  let enumerated = false;

  for (const browserName of options.browsers) {
    const { browser, descriptor } = await launchBrowser(playwright, browserName, options);
    descriptor.label = browserName === "chrome"
      ? `Chrome（${descriptor.headed ? "有头" : "无头"}，${descriptor.version ?? "未启动"}）`
      : `${browserName}（${descriptor.headed ? "有头" : "无头"}，${descriptor.version ?? "未启动"}）`;
    browsers.push(descriptor);
    if (!browser) {
      console.error(`${descriptor.label}：${descriptor.skippedReason}`);
      continue;
    }
    console.error(`启动 ${descriptor.label}`);

    const context = await browser.newContext({
      viewport: { width: 1280, height: 720 },
      locale: "zh-CN",
      ignoreHTTPSErrors: true,
    });
    const page = await context.newPage();
    try {
      await login(page, options);
      if (!enumerated) {
        // The same server/account is reused; enumerate once and reuse the sample list per browser.
        const collected = await collectSamples(page, options);
        samples = collected.samples;
        skippedItems.push(...collected.skippedItems);
        console.error(`媒体库：${collected.libraries.map((library) => library.name).join("、")}；待测 source：${samples.length} 个`);
        enumerated = true;
      }
      if (samples.length === 0) {
        throw new UsageError("没有找到可播放的 source；确认测试库中已同步样本并完成扫描。");
      }
      console.error("");
      let index = 0;
      for (const sample of samples) {
        index += 1;
        let attempt = 0;
        let result = null;
        while (attempt <= options.max429Retries) {
          attempt += 1;
          try {
            result = await measureSample(page, context, sample, options);
            result.attempts = attempt;
            if (options.screenshots && result.status !== "ok" && result.status !== "skipped") {
              try {
                const screenshot = path.join(options.out, "screenshots", `${browserName}-${slugify(sample.title)}.png`);
                await mkdir(path.dirname(screenshot), { recursive: true });
                await page.screenshot({ path: screenshot });
                result.screenshot = path.relative(options.out, screenshot);
              } catch {
                // A screenshot is a diagnostic courtesy; never fail the run for it.
              }
            }
          } catch (error) {
            result = errorMeasurement(sample, error);
            result.attempts = attempt;
          } finally {
            // Always leave the player page before the next sample: it unmounts the player, sends the
            // stop event and frees the single playback slot on the server.
            try {
              await page.goto("about:blank", { waitUntil: "domcontentloaded", timeout: 10000 });
            } catch {
              // The release wait below still gives the server time to reclaim the session.
            }
            await sleep(options.releaseMs);
          }
          if (result.measureError) break;
          if (result.rateLimited && attempt <= options.max429Retries) {
            const delay = [5000, 10000, 20000][Math.min(attempt - 1, 2)];
            console.error(`  ${sample.title}: HTTP 429（并发上限），${delay / 1000}s 后重试（第 ${attempt + 1} 次）`);
            await sleep(delay);
            continue;
          }
          break;
        }
        result.browser = browserName;
        result.browserLabel = descriptor.label;
        result.engine = descriptor.engine;
        if (result.rateLimited) {
          result.status = "skipped";
          result.skippedReason = `HTTP 429 并发上限，重试 ${result.attempts} 次后放弃`;
        }
        if (result.measureError) {
          // A tool-side exception is not a product failure; keep it visible but out of the ❌ count.
          result.status = "skipped";
          result.skippedReason = `测量异常：${result.measureError}`;
        }
        results.push(result);
        const symbol = STATUS_SYMBOL[result.status];
        console.error(`[${index}/${samples.length}] ${symbol} ${sample.title} · ${result.planText}${result.clientFallback ? " · 客户端回退" : ""}`);
      }
    } catch (error) {
      if (error instanceof UsageError) throw error;
      console.error(`${descriptor.label} 运行失败：${error?.message ?? error}`);
      descriptor.skippedReason = `运行中断：${truncate(sanitizeText(error?.message ?? error), 300)}`;
    } finally {
      await context.close().catch(() => undefined);
      await browser.close().catch(() => undefined);
    }
  }

  if (results.length === 0) {
    console.error("没有产生任何测量结果。检查 BROWSER、账号和测试库后重试。");
    return 2;
  }

  const totals = { samples: samples.length, measured: results.filter((result) => result.status !== "skipped").length, ok: 0, warning: 0, failed: 0, skipped: 0 };
  for (const result of results) totals[result.status] += 1;
  const matrix = {};
  for (const browser of browsers.filter((entry) => entry.launched)) {
    matrix[browser.name] = buildMatrix(browser.name, results);
  }
  const report = {
    schemaVersion: 1,
    generatedAt: nowIso(),
    startedAt,
    finishedAt: nowIso(),
    baseUrl: options.baseUrl,
    headless: !options.headed,
    browsers,
    totals,
    matrix,
    skippedItems,
    results: results.map((result) => ({
      browser: result.browser,
      browserLabel: result.browserLabel,
      engine: result.engine,
      sample: result.sample,
      status: result.status,
      plan: result.plan,
      tier: result.tier,
      planText: result.planText,
      fallbacks: result.fallbacks,
      clientFallback: result.clientFallback,
      video: result.video,
      audio: result.audio,
      timing: result.timing,
      blockingAlert: result.blockingAlert,
      mediaError: result.mediaError,
      playError: result.playError,
      progressReset: result.progressReset,
      attempts: result.attempts,
      skippedReason: result.skippedReason,
      measureError: result.measureError ?? null,
      rateLimited: result.rateLimited,
      consoleErrors: result.consoleErrors,
      consolePlayback: result.consolePlayback,
      pageErrors: result.pageErrors,
      failedRequests: result.failedRequests,
      playbackResponses: result.playbackResponses ?? [],
      screenshot: result.screenshot ?? null,
    })),
    suspiciousRules: buildSuspiciousRules(results),
    notes: [
      "报告不包含密码、Cookie、令牌或带签名的媒体 URL。",
      "音频解码仅在 Chromium 引擎通过 webkitAudioDecodedByteCount 判定。",
      "无头 Chrome 的 HEVC/HDR 解码结论可能与真机窗口不同。",
    ],
  };

  const paths = await writeReports(options, report);
  console.error("");
  console.error(`✅ ${totals.ok} · ⚠️ ${totals.warning} · ❌ ${totals.failed} · ⏭️ ${totals.skipped}`);
  console.error(`报告：${paths.markdownPath}`);
  console.error(`JSON：${paths.jsonPath}`);
  return totals.failed > 0 ? 1 : 0;
}

try {
  process.exitCode = await run();
} catch (error) {
  if (error instanceof UsageError) {
    console.error(`错误：${error.message}`);
    process.exitCode = 2;
  } else {
    console.error(`未预期的错误：${error?.stack ?? error}`);
    process.exitCode = 2;
  }
}
