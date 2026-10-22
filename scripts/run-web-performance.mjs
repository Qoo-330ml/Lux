#!/usr/bin/env node
// Measure the Lux Web first-screen and scrolling performance baseline.
//
// The script talks to an already running Lux instance.  Seed a catalog fixture
// with `tools/catalog-fixture/generate.py --catalog-db`, start luxd with that
// config directory, then run:
//
//   NODE_PATH=<playwright node_modules> node scripts/run-web-performance.mjs \
//     --manifest /tmp/lux-perf/manifest.json --runs 3 --output /tmp/lux-perf/baseline.json
//
// M1 first card visible, M2 library items TTFB, M3 first card after a sort
// change, M4 long tasks while scrolling for 30 s, M5 first-screen transfer
// bytes, M6 dropped-frame rate while scrolling.

import { createRequire } from "node:module";
import { parseArgs } from "node:util";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const require = createRequire(import.meta.url);

const { values: args } = parseArgs({
  options: {
    "base-url": { type: "string", default: "http://127.0.0.1:8097" },
    manifest: { type: "string" },
    "session-token": { type: "string" },
    "library-id": { type: "string" },
    runs: { type: "string", default: "3" },
    warmup: { type: "string", default: "1" },
    "cooldown-seconds": { type: "string", default: "16" },
    "restart-command": { type: "string" },
    "download-mbps": { type: "string", default: "20" },
    "upload-mbps": { type: "string", default: "5" },
    "latency-ms": { type: "string", default: "40" },
    "scroll-seconds": { type: "string", default: "30" },
    output: { type: "string" },
    label: { type: "string", default: "" },
    browser: { type: "string" },
    "skip-scroll": { type: "boolean", default: false },
  },
});

const baseUrl = args["base-url"].replace(/\/+$/, "");
const runs = Math.max(1, Number.parseInt(args.runs, 10) || 3);
const warmupRuns = Math.max(0, Number.parseInt(args.warmup, 10) || 0);
const cooldownMs = Math.max(0, Number.parseFloat(args["cooldown-seconds"]) || 0) * 1000;
const restartCommand = args["restart-command"];
const downloadMbps = Math.max(0.1, Number.parseFloat(args["download-mbps"]) || 20);
const uploadMbps = Math.max(0.1, Number.parseFloat(args["upload-mbps"]) || 5);
const latencyMs = Math.max(0, Number.parseFloat(args["latency-ms"]) || 0);
const scrollSeconds = Math.max(3, Number.parseInt(args["scroll-seconds"], 10) || 30);

function readManifest(manifestPath) {
  if (!manifestPath) return {};
  const manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
  const libraries = Array.isArray(manifest.libraries) ? manifest.libraries : [];
  return {
    sessionToken: manifest.sessionToken,
    libraryId: libraries[0]?.id,
    manifest,
  };
}

function playwrightCandidates() {
  const candidates = [];
  try {
    candidates.push(require.resolve("playwright"));
  } catch {
    // fall through to npx caches
  }
  const caches = [
    path.join(os.homedir(), ".npm", "_npx"),
    path.join(os.homedir(), ".cache", "_npx"),
  ];
  for (const cache of caches) {
    let entries = [];
    try {
      entries = fs.readdirSync(cache, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const entry of entries.sort((left, right) => left.name.localeCompare(right.name))) {
      if (!entry.isDirectory()) continue;
      const candidate = path.join(cache, entry.name, "node_modules", "playwright");
      if (fs.existsSync(path.join(candidate, "package.json"))) candidates.push(candidate);
    }
  }
  return [...new Set(candidates)];
}

function browserRevision(candidate) {
  try {
    const core = path.join(path.dirname(candidate), "playwright-core", "browsers.json");
    const browsers = JSON.parse(fs.readFileSync(core, "utf8")).browsers;
    return browsers.find((browser) => browser.name === "chromium")?.revision ?? null;
  } catch {
    return null;
  }
}

function cachedChromiumExists(revision) {
  if (!revision) return false;
  const cache = path.join(os.homedir(), "Library", "Caches", "ms-playwright");
  let entries = [];
  try {
    entries = fs.readdirSync(cache);
  } catch {
    return false;
  }
  return entries.some(
    (entry) => entry === `chromium-${revision}` || entry === `chromium_headless_shell-${revision}`,
  );
}

async function loadPlaywright() {
  const candidates = playwrightCandidates();
  if (!candidates.length) {
    throw new Error(
      "playwright was not found; install it or set NODE_PATH to a node_modules containing playwright",
    );
  }
  const ordered = candidates
    .map((candidate) => ({ candidate, revision: browserRevision(candidate) }))
    .sort((left, right) => {
      const leftReady = cachedChromiumExists(left.revision) ? 0 : 1;
      const rightReady = cachedChromiumExists(right.revision) ? 0 : 1;
      return leftReady - rightReady;
    });
  let lastError;
  for (const { candidate } of ordered) {
    try {
      const playwright = require(candidate);
      // Launching eagerly detects a package/browser revision mismatch before the
      // measurement loop starts.
      const probe = await playwright.chromium.launch({
        headless: true,
        executablePath: args.browser || process.env.PW_BROWSER || undefined,
      });
      await probe.close();
      return { playwright, candidate };
    } catch (error) {
      lastError = error;
    }
  }
  throw lastError ?? new Error("no usable playwright installation found");
}

function median(values) {
  if (!values.length) return null;
  const sorted = [...values].sort((left, right) => left - right);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2;
}

function percentile(values, fraction) {
  if (!values.length) return null;
  const sorted = [...values].sort((left, right) => left - right);
  const index = Math.min(sorted.length - 1, Math.max(0, Math.ceil(fraction * sorted.length) - 1));
  return sorted[index];
}

function summarize(values) {
  const present = values.filter((value) => typeof value === "number" && Number.isFinite(value));
  if (!present.length) return { median: null, p95: null, mean: null, cv: null, samples: [] };
  const mean = present.reduce((total, value) => total + value, 0) / present.length;
  const variance =
    present.reduce((total, value) => total + (value - mean) ** 2, 0) / present.length;
  return {
    median: median(present),
    p95: percentile(present, 0.95),
    mean,
    cv: mean === 0 ? 0 : Math.sqrt(variance) / mean,
    samples: present,
  };
}

const INIT_SCRIPT = () => {
  window.__luxPerf = {
    longTasks: [],
    frames: [],
    firstCard: null,
    sortCard: null,
    sortClick: null,
  };
  try {
    new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) {
        window.__luxPerf.longTasks.push({ startTime: entry.startTime, duration: entry.duration });
      }
    }).observe({ entryTypes: ["longtask"] });
  } catch {
    // longtask is unavailable in some engines; M4 stays null.
  }
  const sampleFrame = (timestamp) => {
    window.__luxPerf.frames.push(timestamp);
    window.requestAnimationFrame(sampleFrame);
  };
  window.requestAnimationFrame(sampleFrame);

  const observeFirstCard = () => {
    const grid = document.querySelector('[aria-label="媒体库内容"]');
    if (!grid) return false;
    if (grid.querySelector('[role="listitem"]')) {
      window.__luxPerf.firstCard = performance.now();
      performance.mark("lux-first-card");
      return true;
    }
    const observer = new MutationObserver(() => {
      if (!grid.querySelector('[role="listitem"]')) return;
      window.__luxPerf.firstCard = performance.now();
      performance.mark("lux-first-card");
      observer.disconnect();
    });
    observer.observe(grid, { childList: true, subtree: true });
    return true;
  };
  document.addEventListener("DOMContentLoaded", () => {
    if (observeFirstCard()) return;
    const observer = new MutationObserver(() => {
      if (observeFirstCard()) observer.disconnect();
    });
    observer.observe(document.documentElement, { childList: true, subtree: true });
  });
};

async function waitForImages(page) {
  await page.evaluate(async () => {
    const images = Array.from(document.querySelectorAll('[aria-label="媒体库内容"] img'));
    await Promise.all(
      images.map((image) => {
        if (image.complete) return Promise.resolve();
        return new Promise((resolve) => {
          image.addEventListener("load", resolve, { once: true });
          image.addEventListener("error", resolve, { once: true });
        });
      }),
    );
  });
}

function categoryFor(response) {
  const mime = response.mimeType || "";
  const url = response.url || "";
  if (mime === "text/html" || response.type === "Document") return "html";
  if (mime.includes("javascript")) return "js";
  if (mime.includes("css")) return "css";
  if (mime.startsWith("font/") || url.includes(".woff")) return "font";
  if (mime.startsWith("image/")) {
    if (url.includes("/images/")) return "image";
    return "asset";
  }
  return "other";
}

async function measureOnce(playwright, options) {
  const browser = await playwright.chromium.launch({
    headless: true,
    executablePath: args.browser || process.env.PW_BROWSER || undefined,
  });
  const context = await browser.newContext({
    viewport: { width: 1280, height: 900 },
    deviceScaleFactor: 1,
  });
  const libraryUrl = `${baseUrl}/libraries/${encodeURIComponent(options.libraryId)}`;
  const cookieUrl = new URL(baseUrl);
  await context.addCookies([
    {
      name: "lux_session",
      value: options.sessionToken,
      domain: cookieUrl.hostname,
      path: "/",
      httpOnly: true,
      sameSite: "Lax",
    },
  ]);

  const page = await context.newPage();
  const consoleErrors = [];
  page.on("console", (message) => {
    if (message.type() === "error") consoleErrors.push(message.text());
  });
  const itemsRequests = [];
  page.on("request", (request) => {
    if (/\/api\/v1\/libraries\/[^/]+\/items\?/.test(request.url())) itemsRequests.push(request.url());
  });

  const cdp = await context.newCDPSession(page);
  await cdp.send("Network.enable");
  await cdp.send("Performance.enable");
  await cdp.send("Network.setCacheDisabled", { cacheDisabled: true });
  await cdp.send("Network.emulateNetworkConditions", {
    offline: false,
    latency: latencyMs,
    downloadThroughput: (downloadMbps * 1024 * 1024) / 8,
    uploadThroughput: (uploadMbps * 1024 * 1024) / 8,
    connectionType: "cellular3g",
  });
  const responses = new Map();
  const encoded = new Map();
  cdp.on("Network.responseReceived", (event) => {
    responses.set(event.requestId, {
      url: event.response.url,
      mimeType: event.response.mimeType,
      status: event.response.status,
      type: event.type,
      timing: event.response.timing,
    });
  });
  cdp.on("Network.loadingFinished", (event) => {
    const response = responses.get(event.requestId);
    if (response) encoded.set(event.requestId, event.encodedDataLength);
  });

  const started = Date.now();
  await page.addInitScript(INIT_SCRIPT);
  await page.goto(libraryUrl, { waitUntil: "commit", timeout: 60000 });
  await page.locator('[aria-label="媒体库内容"] [role="listitem"]').first().waitFor({ timeout: 60000 });
  await waitForImages(page);
  await page.waitForTimeout(250);

  const first = await page.evaluate(() => {
    const navigation = performance.getEntriesByType("navigation")[0];
    const resources = performance.getEntriesByType("resource").map((entry) => ({
      name: entry.name,
      initiatorType: entry.initiatorType,
      startTime: entry.startTime,
      responseStart: entry.responseStart,
      duration: entry.duration,
      transferSize: entry.transferSize,
      encodedBodySize: entry.encodedBodySize,
      decodedBodySize: entry.decodedBodySize,
    }));
    const items = resources.find((entry) => /\/api\/v1\/libraries\/[^/]+\/items\?/.test(entry.name));
    return {
      firstCard: window.__luxPerf.firstCard,
      longTasks: window.__luxPerf.longTasks,
      frames: window.__luxPerf.frames,
      navigation: navigation
        ? {
            responseStart: navigation.responseStart,
            duration: navigation.duration,
            transferSize: navigation.transferSize,
            encodedBodySize: navigation.encodedBodySize,
          }
        : null,
      items,
      resources,
      viewport: { width: window.innerWidth, height: window.innerHeight },
    };
  });

  // M3: switch the sort order and time the first rebuilt card.  The grid is
  // keyed by the sort query, so observe the document instead of the old grid.
  const sortMeasurement = await page.evaluate(async () => {
    const cardLabel = () =>
      document
        .querySelector('[aria-label="媒体库内容"] [role="listitem"] a[aria-label^="查看"]')
        ?.getAttribute("aria-label") ?? null;
    const before = cardLabel();
    window.__luxPerf.sortCard = null;
    window.__luxPerf.sortBefore = before;
    if (!before) return null;
    const trigger = document.querySelector('[role="combobox"][aria-label="排序方式"]');
    if (!trigger) return null;
    trigger.click();
    const observer = new MutationObserver(() => {
      const label = cardLabel();
      if (!label || label === before) return;
      window.__luxPerf.sortCard = performance.now();
      performance.mark("lux-sort-first-card");
      observer.disconnect();
    });
    observer.observe(document.body, { childList: true, subtree: true, attributes: true, characterData: true });
    return true;
  });
  let sortFirstCard = null;
  let sortClick = null;
  if (sortMeasurement) {
    const option = page.getByRole("option", { name: "评分", exact: true });
    await option.waitFor({ timeout: 10000 });
    sortClick = await page.evaluate(() => {
      performance.mark("lux-sort-click");
      return performance.now();
    });
    await option.click();
    try {
      await page.waitForFunction(
        () => typeof window.__luxPerf.sortCard === "number" && window.__luxPerf.sortCard > 0,
        { timeout: 15000 },
      );
    } catch {
      // keep the metric null when the grid does not rebuild
    }
    sortFirstCard = await page.evaluate(() => window.__luxPerf.sortCard);
  }

  const scrollWindow = { start: 0, end: 0 };
  if (!options.skipScroll && !args["skip-scroll"]) {
    scrollWindow.start = await page.evaluate(() => {
      window.scrollTo(0, 0);
      const start = performance.now();
      window.__luxPerf.frames = [];
      window.__luxPerf.longTasks = [];
      return start;
    });
    const startedAt = Date.now();
    while (Date.now() - startedAt < scrollSeconds * 1000) {
      await page.evaluate(() => window.scrollBy(0, 720));
      await page.waitForTimeout(110);
    }
    scrollWindow.end = await page.evaluate(() => performance.now());
  }
  const scrolled = await page.evaluate(() => ({
    longTasks: window.__luxPerf.longTasks,
    frames: window.__luxPerf.frames,
    scrollY: window.scrollY,
    documentHeight: document.documentElement.scrollHeight,
  }));

  const end = Date.now();
  const firstScreenBytes = { html: 0, js: 0, css: 0, font: 0, image: 0, asset: 0, other: 0 };
  const firstScreenRequests = [];
  let firstScreenImages = 0;
  for (const [requestId, response] of responses) {
    const bytes = encoded.get(requestId) ?? 0;
    let url;
    try {
      url = new URL(response.url);
    } catch {
      continue;
    }
    if (url.origin !== cookieUrl.origin) continue;
    const category = categoryFor(response);
    if (category === "image") {
      if (!/\/api\/v1\/items\/[^/]+\/images\//.test(url.pathname)) continue;
      if (firstScreenImages >= 24) continue;
      firstScreenImages += 1;
    }
    firstScreenBytes[category] = (firstScreenBytes[category] ?? 0) + bytes;
    firstScreenRequests.push({ category, status: response.status, bytes, url: url.pathname });
  }
  const measuredBytes =
    firstScreenBytes.html +
    firstScreenBytes.js +
    firstScreenBytes.css +
    firstScreenBytes.font +
    firstScreenBytes.image;

  const frameIntervals = [];
  for (let index = 1; index < scrolled.frames.length; index += 1) {
    const interval = scrolled.frames[index] - scrolled.frames[index - 1];
    if (interval >= 0) frameIntervals.push(interval);
  }
  const frameMedian = median(frameIntervals);
  const droppedFrames = frameMedian
    ? frameIntervals.filter((interval) => interval > Math.max(24, frameMedian * 1.5)).length
    : 0;
  const longTasks = scrolled.longTasks.filter(
    (entry) => entry.startTime >= scrollWindow.start && entry.startTime <= scrollWindow.end,
  );
  const longTaskTotal = longTasks.reduce((total, entry) => total + entry.duration, 0);

  const result = {
    collectedAt: new Date().toISOString(),
    wallClockMs: end - started,
    baseUrl,
    libraryId: options.libraryId,
    viewport: first.viewport,
    m1: first.firstCard,
    m2: first.items?.responseStart ?? null,
    m2Duration: first.items?.duration ?? null,
    m3: sortClick != null && sortFirstCard != null ? sortFirstCard - sortClick : null,
    m4: longTasks.length,
    m4TotalMs: longTaskTotal,
    m5: measuredBytes,
    m5Bytes: firstScreenBytes,
    m5Images: firstScreenImages,
    m6: frameIntervals.length ? (droppedFrames / frameIntervals.length) * 100 : null,
    frames: frameIntervals.length,
    droppedFrames,
    scrollY: scrolled.scrollY,
    itemsRequests: itemsRequests.length,
    itemsTransferSize: first.items?.transferSize ?? null,
    itemsEncodedBodySize: first.items?.encodedBodySize ?? null,
    navigation: first.navigation,
    firstScreenRequests,
    consoleErrors,
    scrollYAfter: scrolled.scrollY,
    documentHeight: scrolled.documentHeight,
  };

  await browser.close();
  return result;
}


async function restartServer() {
  if (!restartCommand) return;
  const { execSync } = require("node:child_process");
  execSync(restartCommand, { stdio: "inherit", shell: true });
  const deadline = Date.now() + 60000;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(`${baseUrl}/`);
      if (response.ok) return;
    } catch {
      // server is still starting
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error("server did not become ready after restart");
}

async function main() {
  const manifest = readManifest(args.manifest);
  const sessionToken = args["session-token"] || manifest.sessionToken;
  const libraryId = args["library-id"] || manifest.libraryId;
  if (!sessionToken) throw new Error("--session-token or a fixture manifest is required");
  if (!libraryId) throw new Error("--library-id or a fixture manifest is required");

  const { playwright, candidate } = await loadPlaywright();
  const results = [];
  for (let warmup = 0; warmup < warmupRuns; warmup += 1) {
    await restartServer();
    await measureOnce(playwright, { sessionToken, libraryId, skipScroll: true });
    process.stderr.write(`warmup ${warmup + 1}/${warmupRuns} complete\n`);
  }
  for (let run = 0; run < runs; run += 1) {
    await restartServer();
    if (cooldownMs > 0) await new Promise((resolve) => setTimeout(resolve, cooldownMs));
    const result = await measureOnce(playwright, { sessionToken, libraryId });
    results.push(result);
    process.stderr.write(
      `run ${run + 1}/${runs}: M1=${fmt(result.m1)} M2=${fmt(result.m2)} M3=${fmt(result.m3)} ` +
        `M4=${result.m4} M5=${result.m5} M6=${fmt(result.m6)}%\n`,
    );
  }

  const summary = {
    m1: summarize(results.map((run) => run.m1)),
    m2: summarize(results.map((run) => run.m2)),
    m3: summarize(results.map((run) => run.m3)),
    m4: summarize(results.map((run) => run.m4)),
    m5: summarize(results.map((run) => run.m5)),
    m6: summarize(results.map((run) => run.m6)),
  };
  const report = {
    label: args.label || null,
    baseUrl,
    commit: gitCommit(),
    collectedAt: new Date().toISOString(),
    playwright: candidate,
    runs,
    warmupRuns,
    cooldownSeconds: cooldownMs / 1000,
    restartedPerRun: Boolean(restartCommand),
    network: { downloadMbps, uploadMbps, latencyMs },
    scrollSeconds,
    summary,
    results,
  };
  const serialized = `${JSON.stringify(report, null, 2)}\n`;
  if (args.output) {
    fs.mkdirSync(path.dirname(path.resolve(args.output)), { recursive: true });
    fs.writeFileSync(args.output, serialized);
  }
  process.stdout.write(serialized);
}

function fmt(value) {
  return typeof value === "number" && Number.isFinite(value) ? value.toFixed(1) : "-";
}

function gitCommit() {
  try {
    return require("node:child_process")
      .execSync("git rev-parse --short HEAD", { encoding: "utf8" })
      .trim();
  } catch {
    return null;
  }
}

await main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
