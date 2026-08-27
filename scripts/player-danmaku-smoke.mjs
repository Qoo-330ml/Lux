import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const { chromium } = require("playwright");

const baseUrl = process.env.LUX_E2E_BASE_URL || "http://127.0.0.1:8097";
const username = process.env.LUX_E2E_USERNAME || "viewer";
const password = process.env.LUX_E2E_PASSWORD;
const itemId = process.env.LUX_E2E_ITEM;
const sourceId = process.env.LUX_E2E_SOURCE_ID || "";
if (!password) throw new Error("LUX_E2E_PASSWORD is required");
if (!itemId) throw new Error("LUX_E2E_ITEM is required");

const browser = await chromium.launch({
  headless: true,
  executablePath: process.env.PW_BROWSER || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
});
const page = await browser.newPage({ viewport: { width: 390, height: 844 } });
const consoleErrors = [];
const consoleWarnings = [];
const pageErrors = [];
let captureBrowserDiagnostics = false;
const externalRequests = [];
const compatibilityDanmakuRequests = [];
const danmakuResponses = [];

page.on("console", (message) => {
  if (!captureBrowserDiagnostics) return;
  if (message.type() === "error") consoleErrors.push(message.text());
  if (message.type() === "warning") consoleWarnings.push(message.text());
});
page.on("pageerror", (error) => {
  if (captureBrowserDiagnostics) pageErrors.push(error.message);
});
page.on("request", (request) => {
  const url = new URL(request.url());
  if (url.origin !== new URL(baseUrl).origin) externalRequests.push(url.origin);
  if (url.pathname.startsWith("/api/danmu/")) compatibilityDanmakuRequests.push(url.pathname);
  if (url.pathname.includes("/danmaku")) {
    danmakuResponses.push({ method: request.method(), path: url.pathname, status: null });
  }
});
page.on("response", (response) => {
  const url = new URL(response.url());
  if (url.pathname.includes("/danmaku")) {
    const match = [...danmakuResponses].reverse().find((entry) => entry.path === url.pathname && entry.status === null);
    if (match) match.status = response.status();
  }
});

await page.goto(`${baseUrl}/`, { waitUntil: "networkidle" });
await page.getByLabel("用户名", { exact: true }).fill(username);
await page.getByLabel("密码", { exact: true }).fill(password);
await Promise.all([
  page.waitForResponse((response) => new URL(response.url()).pathname === "/api/v1/auth/login" && response.status() === 200),
  page.getByRole("button", { name: "进入 Lux", exact: true }).click(),
]);
captureBrowserDiagnostics = true;

const playerUrl = `${baseUrl}/watch/${encodeURIComponent(itemId)}${sourceId ? `?sourceId=${encodeURIComponent(sourceId)}` : ""}`;
await page.goto(playerUrl, { waitUntil: "networkidle" });
await page.locator(".lux-player-page").waitFor({ state: "visible" });
await page.locator("video.lux-video").waitFor({ state: "attached" });
await page.locator("[data-lux-danmaku-overlay]").waitFor({ state: "attached" });
await page.waitForFunction(() => document.querySelectorAll("[data-lux-danmaku-overlay] .lux-player-danmaku-text").length >= 0);

const video = page.locator("video.lux-video");
const timeline = page.getByRole("slider", { name: "播放进度", exact: true });
const timelineBox = await timeline.boundingBox();
if (!timelineBox) throw new Error("playback timeline is not measurable");
const timeBeforeSeek = await video.evaluate((element) => element.currentTime);
await timeline.click({ position: { x: timelineBox.width * 0.6, y: timelineBox.height / 2 } });
await page.waitForFunction((before) => {
  const currentTime = document.querySelector("video.lux-video")?.currentTime ?? 0;
  return currentTime > before + 0.25;
}, timeBeforeSeek);

const safeZone = await page.evaluate(() => {
  const topbar = document.querySelector(".lux-player-topbar")?.getBoundingClientRect();
  const controls = document.querySelector(".lux-player-controls-wrap")?.getBoundingClientRect();
  if (!topbar || !controls) return false;
  return [...document.querySelectorAll(".lux-player-danmaku-text")].every((element) => {
    const bounds = element.getBoundingClientRect();
    return bounds.top >= topbar.bottom && bounds.bottom <= controls.top;
  });
});
if (!safeZone) throw new Error("danmaku overlaps player chrome");

const toggle = page.getByRole("button", { name: "隐藏弹幕", exact: true });
await toggle.waitFor({ state: "visible" });
const requestsBeforeHide = danmakuResponses.length;
await toggle.click();
await page.getByRole("button", { name: "显示弹幕", exact: true }).waitFor();
if (danmakuResponses.length !== requestsBeforeHide) throw new Error("hiding danmaku created a network request");
if (await page.locator("[data-lux-danmaku-overlay]").count()) throw new Error("hidden danmaku left an overlay in the DOM");

await page.getByRole("button", { name: "显示弹幕", exact: true }).click();
await page.getByRole("button", { name: "隐藏弹幕", exact: true }).waitFor();
await video.evaluate((element) => {
  element.currentTime = Math.min(10, Number.isFinite(element.duration) ? element.duration : 10);
  element.dispatchEvent(new Event("timeupdate"));
});

const viewportChecks = [];
for (const viewport of [{ width: 390, height: 844 }, { width: 768, height: 1024 }, { width: 1440, height: 900 }]) {
  await page.setViewportSize(viewport);
  viewportChecks.push({
    ...viewport,
    noHorizontalOverflow: await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth),
    controlsNamed: await page.getByRole("button", { name: "播放器设置", exact: true }).count() === 1
      && await page.getByRole("button", { name: "截图", exact: true }).count() === 1
      && await page.getByRole("button", { name: "画中画", exact: true }).count() === 1
      && await page.getByRole("button", { name: /全屏/ }).count() === 1,
  });
}

const result = {
  viewportChecks,
  danmakuResponses: danmakuResponses.map(({ method, path, status }) => ({ method, path, status })),
  hasRawRead: danmakuResponses.some(({ path, status }) => path.endsWith("/danmaku/raw") && status === 200),
  safeZone,
  compatibilityDanmakuRequests,
  externalRequests,
  consoleErrors,
  consoleWarnings,
  pageErrors,
};
console.log(JSON.stringify(result, null, 2));
await browser.close();

if (
  !result.hasRawRead
  || !result.safeZone
  || result.compatibilityDanmakuRequests.length > 0
  || result.externalRequests.length > 0
  || result.consoleErrors.length > 0
  || result.consoleWarnings.length > 0
  || result.pageErrors.length > 0
  || result.viewportChecks.some((check) => !check.noHorizontalOverflow || !check.controlsNamed)
) process.exitCode = 1;
