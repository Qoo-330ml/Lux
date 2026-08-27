import { mkdirSync } from "node:fs";
import { createRequire } from "node:module";
import { join } from "node:path";

const require = createRequire(import.meta.url);
const { chromium } = require("playwright");

const baseUrl = process.env.LUX_E2E_BASE_URL || "http://127.0.0.1:8097";
const username = process.env.LUX_E2E_USERNAME || "viewer";
const password = process.env.LUX_E2E_PASSWORD;
const itemId = process.env.LUX_E2E_ITEM;
const sourceId = process.env.LUX_E2E_SOURCE_ID || "";
const switchSourceId = process.env.LUX_E2E_SWITCH_SOURCE_ID || "";
const forceHls = process.env.LUX_E2E_FORCE_HLS === "1";
const forceClientHevc = process.env.LUX_E2E_FORCE_CLIENT_HEVC === "1";
const expectedCaption = process.env.LUX_E2E_EXPECT_CAPTION || "";
const expectedDanmaku = process.env.LUX_E2E_EXPECT_DANMAKU || "Stage 17 fixture";
const switchExpectedCaption = process.env.LUX_E2E_SWITCH_EXPECT_CAPTION || "";
const switchExpectedDanmaku = process.env.LUX_E2E_SWITCH_EXPECT_DANMAKU || "";
const stage18 = process.env.LUX_E2E_STAGE18 === "1";
const expectedChapter = process.env.LUX_E2E_EXPECT_CHAPTER || "";
const expectedIntroStart = Number(process.env.LUX_E2E_EXPECT_INTRO_START);
const expectedIntroEnd = Number(process.env.LUX_E2E_EXPECT_INTRO_END);
const switchExpectedChapter = process.env.LUX_E2E_SWITCH_EXPECT_CHAPTER || "";
const airPlayMode = process.env.LUX_E2E_AIRPLAY_MODE || "none";
const artifactDir = process.env.LUX_E2E_ARTIFACT_DIR || "";
if (!password) throw new Error("LUX_E2E_PASSWORD is required");
if (!itemId) throw new Error("LUX_E2E_ITEM is required");
if (forceHls && forceClientHevc) throw new Error("LUX_E2E_FORCE_HLS and LUX_E2E_FORCE_CLIENT_HEVC are mutually exclusive");
if (switchSourceId && switchSourceId === sourceId) throw new Error("LUX_E2E_SWITCH_SOURCE_ID must differ from LUX_E2E_SOURCE_ID");
if (switchSourceId && (!switchExpectedCaption || !switchExpectedDanmaku)) {
  throw new Error("source switching requires distinct expected caption and danmaku text");
}
if (stage18 && (!expectedChapter || !Number.isFinite(expectedIntroStart) || !Number.isFinite(expectedIntroEnd))) {
  throw new Error("stage 18 requires a chapter title and finite intro start/end seconds");
}
if (stage18 && expectedIntroEnd <= expectedIntroStart) {
  throw new Error("stage 18 intro end must be greater than intro start");
}
if (stage18 && switchSourceId && !switchExpectedChapter) {
  throw new Error("stage 18 source switching requires a distinct expected chapter title");
}
if (stage18 && switchSourceId && switchExpectedChapter === expectedChapter) {
  throw new Error("stage 18 source switching requires a distinct expected chapter title");
}
if (stage18 && !["available", "unavailable"].includes(airPlayMode)) {
  throw new Error("stage 18 requires LUX_E2E_AIRPLAY_MODE=available or unavailable");
}
if (artifactDir) mkdirSync(artifactDir, { recursive: true });

const browser = await chromium.launch({
  headless: true,
  executablePath: process.env.PW_BROWSER || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
});
const context = await browser.newContext({ viewport: { width: 390, height: 844 }, hasTouch: true });
const page = await context.newPage();
const consoleErrors = [];
const consoleWarnings = [];
const pageErrors = [];
let captureBrowserDiagnostics = false;
let capturePlayerRequests = false;
const externalRequests = [];
const compatibilityDanmakuRequests = [];
const playerRequests = [];
const danmakuResponses = [];
const playbackPlans = [];
const playbackPlanReads = [];
const hlsResponses = [];
const hevcResponses = [];
const captionResponses = [];
const sessionRequests = [];
const sessionResponses = [];
const sessionStopResponses = [];

page.on("console", (message) => {
  if (!captureBrowserDiagnostics) return;
  if (message.type() === "error") {
    const source = message.location().url;
    const sourcePath = source ? new URL(source).pathname : "";
    consoleErrors.push(sourcePath ? `${message.text()} @ ${sourcePath}` : message.text());
  }
  if (message.type() === "warning") consoleWarnings.push(message.text());
});
page.on("pageerror", (error) => {
  if (captureBrowserDiagnostics) pageErrors.push(error.message);
});
page.on("request", (request) => {
  const url = new URL(request.url());
  if (url.origin !== new URL(baseUrl).origin) externalRequests.push(url.origin);
  if (capturePlayerRequests) playerRequests.push({ method: request.method(), path: url.pathname });
  if (url.pathname.startsWith("/api/v1/playback/sessions")) {
    let state = null;
    if (url.pathname.endsWith("/events")) {
      try {
        state = request.postDataJSON()?.state ?? null;
      } catch {
        state = null;
      }
    }
    sessionRequests.push({ method: request.method(), path: url.pathname, state });
  }
  if (url.pathname.startsWith("/api/danmu/")) compatibilityDanmakuRequests.push(url.pathname);
  if (url.pathname.includes("/danmaku")) {
    danmakuResponses.push({
      method: request.method(),
      path: url.pathname,
      sourceId: url.searchParams.get("sourceId"),
      status: null,
    });
  }
  if (url.pathname.includes("/api/v1/playback/sessions/") && url.pathname.includes("/hls/")) {
    hlsResponses.push({ method: request.method(), path: url.pathname, status: null });
  }
  if (url.pathname.startsWith("/hevc/")) {
    hevcResponses.push({ method: request.method(), path: url.pathname, status: null });
  }
  if (url.pathname.includes("/subtitles/")) {
    captionResponses.push({ method: request.method(), path: url.pathname, status: null });
  }
});
page.on("response", (response) => {
  const url = new URL(response.url());
  if (url.pathname.startsWith("/api/v1/playback/sessions")) {
    let state = null;
    if (url.pathname.endsWith("/events")) {
      try {
        state = response.request().postDataJSON()?.state ?? null;
      } catch {
        state = null;
      }
    }
    sessionResponses.push({ method: response.request().method(), path: url.pathname, state, status: response.status() });
  }
  if (url.pathname === "/api/v1/playback/sessions" && response.request().method() === "POST") {
    playbackPlanReads.push(response.json().then((body) => {
      playbackPlans.push({ sessionId: body?.sessionId || null, type: body?.plan?.type || null });
    }).catch(() => undefined));
  }
  if (
    response.request().method() === "DELETE"
    && /^\/api\/v1\/playback\/sessions\/[^/]+$/.test(url.pathname)
  ) {
    sessionStopResponses.push({ path: url.pathname, status: response.status() });
  }
  if (url.pathname.includes("/danmaku")) {
    const match = [...danmakuResponses].reverse().find((entry) => (
      entry.path === url.pathname
      && entry.sourceId === url.searchParams.get("sourceId")
      && entry.status === null
    ));
    if (match) match.status = response.status();
  }
  if (url.pathname.includes("/api/v1/playback/sessions/") && url.pathname.includes("/hls/")) {
    const match = [...hlsResponses].reverse().find((entry) => entry.path === url.pathname && entry.status === null);
    if (match) match.status = response.status();
  }
  if (url.pathname.startsWith("/hevc/")) {
    const match = [...hevcResponses].reverse().find((entry) => entry.path === url.pathname && entry.status === null);
    if (match) match.status = response.status();
  }
  if (url.pathname.includes("/subtitles/")) {
    const match = [...captionResponses].reverse().find((entry) => entry.path === url.pathname && entry.status === null);
    if (match) match.status = response.status();
  }
});

if (stage18 && airPlayMode === "available") {
  await page.addInitScript(() => {
    Object.defineProperty(window, "WebKitPlaybackTargetAvailabilityEvent", {
      configurable: true,
      value: function WebKitPlaybackTargetAvailabilityEvent() { return undefined; },
    });
    Object.defineProperty(HTMLMediaElement.prototype, "webkitShowPlaybackTargetPicker", {
      configurable: true,
      value() {
        this.dataset.luxAirplayPickerCalls = String(Number(this.dataset.luxAirplayPickerCalls || 0) + 1);
      },
    });
  });
}
if (stage18 && airPlayMode === "unavailable") {
  await page.addInitScript(() => {
    delete window.WebKitPlaybackTargetAvailabilityEvent;
    delete HTMLMediaElement.prototype.webkitShowPlaybackTargetPicker;
  });
}

await page.goto(`${baseUrl}/`, { waitUntil: "networkidle" });
await page.getByLabel("用户名", { exact: true }).fill(username);
await page.getByLabel("密码", { exact: true }).fill(password);
await Promise.all([
  page.waitForResponse((response) => new URL(response.url()).pathname === "/api/v1/auth/login" && response.status() === 200),
  page.getByRole("button", { name: "进入 Lux", exact: true }).click(),
]);
captureBrowserDiagnostics = true;

const playerUrl = `${baseUrl}/watch/${encodeURIComponent(itemId)}${sourceId ? `?sourceId=${encodeURIComponent(sourceId)}` : ""}`;
const detailUrl = `${baseUrl}/items/${encodeURIComponent(itemId)}${sourceId ? `?sourceId=${encodeURIComponent(sourceId)}` : ""}`;
// HLS keeps requesting playlists/segments while playing, so networkidle is
// not a stable readiness signal for the player route.
if (forceHls) {
  await page.route("**/api/v1/playback/sessions/*/direct**", (route) => route.fulfill({
    status: 200,
    contentType: "video/mp4",
    body: "lux-e2e-direct-unavailable",
  }));
}
if (forceClientHevc) {
  await page.addInitScript(() => {
    const originalCanPlayType = HTMLMediaElement.prototype.canPlayType;
    HTMLMediaElement.prototype.canPlayType = function canPlayType(type) {
      if (/\b(?:hvc1|hev1|hevc)\b/i.test(type)) return "";
      return originalCanPlayType.call(this, type);
    };
  });
}
await page.goto(detailUrl, { waitUntil: "domcontentloaded" });
await page.getByRole("link", { name: "播放", exact: true }).waitFor({ state: "visible" });
capturePlayerRequests = true;
await page.getByRole("link", { name: "播放", exact: true }).click();
await page.waitForURL(playerUrl);
await page.locator(".lux-player-page").waitFor({ state: "visible" });
await page.locator("video.lux-video").waitFor({ state: "attached" });
await page.locator("[data-lux-danmaku-overlay]").waitFor({ state: "attached" });
const selectCaption = async () => {
  if (!expectedCaption) return;
  await page.locator(".lux-player-page").hover();
  await page.getByRole("button", { name: "播放器设置", exact: true }).click();
  const captionSelector = page.getByLabel("选择字幕", { exact: true });
  await captionSelector.waitFor({ state: "visible" });
  const captionValue = await captionSelector.locator("option:not([value='']):not([disabled])").first().getAttribute("value");
  if (!captionValue) throw new Error("expected a selectable text caption track");
  await captionSelector.selectOption(captionValue);
  await page.getByRole("button", { name: "关闭播放设置", exact: true }).click();
};
await selectCaption();

const video = page.locator("video.lux-video");
await page.waitForFunction(() => {
  const element = document.querySelector("video.lux-video");
  return Boolean(element && element.readyState >= 2 && Number.isFinite(element.duration) && element.duration > 0);
}, undefined, { timeout: 30_000 });
const timeline = page.getByRole("slider", { name: "播放进度", exact: true });
const timelineBox = await timeline.boundingBox();
if (!timelineBox) throw new Error("playback timeline is not measurable");

const dispatchAirPlayAvailability = (target, availability) => target.evaluate((element, value) => {
  element.dispatchEvent(Object.assign(new Event("webkitplaybacktargetavailabilitychanged"), { availability: value }));
}, availability);

let stage18Settings = { checked: false, sessionCreatesUnchanged: true, presentationApplied: false, offsetApplied: false };
let stage18AirPlay = { mode: airPlayMode, availableShown: false, pickerCalled: false, unavailableHidden: false, sessionCreatesUnchanged: true };
let stage18Chapters = { checked: false, chapterSeek: false, introSkip: false, creditsVisible: false };
let stage18MiniProgress = { checked: false, hidden: false, nonInteractive: false, reflectsProgress: false, removedOnActivity: false };
if (stage18) {
  await page.locator(".lux-player-page").hover();
  await page.getByRole("button", { name: "播放器设置", exact: true }).click();
  const settingsDialog = page.getByRole("dialog", { name: "播放设置", exact: true });
  await settingsDialog.waitFor({ state: "visible" });
  const sessionCreatesBeforeSettings = sessionRequests.filter((request) => (
    request.method === "POST" && request.path === "/api/v1/playback/sessions"
  )).length;
  await settingsDialog.getByRole("switch", { name: "循环播放", exact: true }).click();
  await settingsDialog.getByRole("button", { name: "4:3", exact: true }).click();
  await settingsDialog.getByRole("button", { name: "水平镜像", exact: true }).click();
  const offsetInput = settingsDialog.locator("#lux-player-caption-offset");
  await offsetInput.fill("1.2");
  await page.waitForTimeout(100);
  const presentation = await video.evaluate((element) => ({
    loop: element.loop,
    aspectRatio: element.style.aspectRatio,
    transform: element.style.transform,
  }));
  stage18Settings = {
    checked: true,
    sessionCreatesUnchanged: sessionRequests.filter((request) => (
      request.method === "POST" && request.path === "/api/v1/playback/sessions"
    )).length === sessionCreatesBeforeSettings,
    presentationApplied: presentation.loop
      && presentation.aspectRatio === "4 / 3"
      && presentation.transform === "translate(-50%, -50%) scaleX(-1)",
    offsetApplied: await offsetInput.inputValue() === "1.2",
  };
  await settingsDialog.getByRole("switch", { name: "循环播放", exact: true }).click();
  await settingsDialog.getByRole("button", { name: "默认", exact: true }).click();
  await settingsDialog.getByRole("button", { name: "正常", exact: true }).click();
  await offsetInput.fill("0");
  await settingsDialog.getByRole("button", { name: "关闭播放设置", exact: true }).click();

  const chapter = page.getByRole("button", { name: `章节：${expectedChapter}`, exact: true });
  await chapter.waitFor({ state: "visible" });
  await chapter.focus();
  const chapterPresentation = await chapter.evaluate((element) => {
    const bounds = element.getBoundingClientRect();
    return {
      title: element.getAttribute("title") || "",
      focused: document.activeElement === element,
      inViewport: bounds.left >= 0 && bounds.right <= window.innerWidth
        && bounds.top >= 0 && bounds.bottom <= window.innerHeight,
      start: Number(element.getAttribute("data-chapter-start")),
    };
  });
  if (!chapterPresentation.title || !chapterPresentation.focused || !chapterPresentation.inViewport) {
    throw new Error(`chapter is not focusable/titleable: ${JSON.stringify(chapterPresentation)}`);
  }
  const sessionCreatesBeforeChapterSeek = sessionRequests.filter((request) => (
    request.method === "POST" && request.path === "/api/v1/playback/sessions"
  )).length;
  await chapter.click();
  await page.waitForFunction(({ target }) => {
    const element = document.querySelector("video.lux-video");
    return Boolean(element && Math.abs(element.currentTime - target) <= 0.5);
  }, { target: chapterPresentation.start });
  const chapterSeek = sessionRequests.filter((request) => (
    request.method === "POST" && request.path === "/api/v1/playback/sessions"
  )).length === sessionCreatesBeforeChapterSeek;

  await video.evaluate((element) => element.pause());
  const introTime = expectedIntroStart + (expectedIntroEnd - expectedIntroStart) / 2;
  await video.evaluate((element, target) => {
    element.currentTime = target;
    element.dispatchEvent(new Event("timeupdate"));
  }, introTime);
  const skipIntro = page.getByRole("button", { name: "跳过片头", exact: true });
  await skipIntro.waitFor({ state: "visible" });
  await skipIntro.click();
  await page.waitForFunction((target) => (
    (document.querySelector("video.lux-video")?.currentTime ?? 0) >= target - 0.5
  ), expectedIntroEnd);
  stage18Chapters = {
    checked: true,
    chapterSeek,
    introSkip: true,
    creditsVisible: await page.locator("[data-marker-type='CREDITS_START']").count() > 0,
  };

  const sessionCreatesBeforeAirPlay = sessionRequests.filter((request) => (
    request.method === "POST" && request.path === "/api/v1/playback/sessions"
  )).length;
  if (airPlayMode === "available") {
    await dispatchAirPlayAvailability(video, "available");
    const airPlayButton = page.getByRole("button", { name: "AirPlay", exact: true });
    await airPlayButton.waitFor({ state: "visible" });
    stage18AirPlay.availableShown = true;
    await airPlayButton.click();
    stage18AirPlay.pickerCalled = await video.evaluate((element) => element.dataset.luxAirplayPickerCalls === "1");
    await dispatchAirPlayAvailability(video, "not-available");
    await airPlayButton.waitFor({ state: "detached" });
    stage18AirPlay.unavailableHidden = true;
  } else {
    stage18AirPlay.unavailableHidden = await page.getByRole("button", { name: "AirPlay", exact: true }).count() === 0;
  }
  stage18AirPlay.sessionCreatesUnchanged = sessionRequests.filter((request) => (
    request.method === "POST" && request.path === "/api/v1/playback/sessions"
  )).length === sessionCreatesBeforeAirPlay;
}
const durationBeforeSeek = await video.evaluate((element) => element.duration);
const seekTarget = durationBeforeSeek * 0.6;
await timeline.click({ position: { x: timelineBox.width * 0.6, y: timelineBox.height / 2 } });
await page.waitForFunction(({ target }) => {
  const element = document.querySelector("video.lux-video");
  if (!element) return false;
  return Math.abs(element.currentTime - target) <= Math.max(0.25, element.duration * 0.05);
}, { target: seekTarget });
if (expectedCaption) {
  await page.locator(".lux-player-caption-text", { hasText: expectedCaption }).waitFor({ state: "visible" });
}
await page.locator(".lux-player-danmaku-text", { hasText: expectedDanmaku }).waitFor({ state: "visible" });

let sourceSwitch = { requested: false, stoppedPreviousSession: true, singleOverlay: true, selectedSourceId: sourceId };
if (switchSourceId) {
  const stopsBeforeSwitch = sessionStopResponses.length;
  const sessionResponse = page.waitForResponse((response) => (
    new URL(response.url()).pathname === "/api/v1/playback/sessions"
    && response.request().method() === "POST"
    && response.status() === 200
  ));
  const danmakuResponse = page.waitForResponse((response) => {
    const url = new URL(response.url());
    return url.pathname.endsWith("/danmaku")
      && url.searchParams.get("sourceId") === switchSourceId
      && response.status() === 200;
  });
  const stopResponse = page.waitForResponse((response) => (
    response.request().method() === "DELETE"
    && /^\/api\/v1\/playback\/sessions\/[^/]+$/.test(new URL(response.url()).pathname)
    && response.status() === 204
  ));
  await page.locator(".lux-player-page").hover();
  await page.getByLabel("选择播放版本", { exact: true }).selectOption(switchSourceId);
  await Promise.all([sessionResponse, danmakuResponse, stopResponse]);
  await page.waitForFunction((expectedSourceId) => (
    new URL(window.location.href).searchParams.get("sourceId") === expectedSourceId
  ), switchSourceId);
  await page.waitForFunction(() => {
    const element = document.querySelector("video.lux-video");
    return Boolean(element && element.readyState >= 2 && Number.isFinite(element.duration) && element.duration > 0);
  }, undefined, { timeout: 30_000 });
  await selectCaption();
  if (stage18) {
    await page.getByRole("button", { name: `章节：${switchExpectedChapter}`, exact: true }).waitFor({ state: "visible" });
    if (await page.getByRole("button", { name: `章节：${expectedChapter}`, exact: true }).count() !== 0) {
      throw new Error("source switching left the previous chapter visible");
    }
    if (await page.getByRole("button", { name: "跳过片头", exact: true }).count() !== 0) {
      throw new Error("source switching left the previous intro skip visible");
    }
    if (airPlayMode === "available") {
      const switchedVideo = page.locator("video.lux-video");
      await dispatchAirPlayAvailability(switchedVideo, "available");
      await page.getByRole("button", { name: "AirPlay", exact: true }).waitFor({ state: "visible" });
      await dispatchAirPlayAvailability(switchedVideo, "not-available");
      await page.getByRole("button", { name: "AirPlay", exact: true }).waitFor({ state: "detached" });
    }
  }
  await timeline.click({ position: { x: timelineBox.width * 0.3, y: timelineBox.height / 2 } });
  await page.locator(".lux-player-caption-text", { hasText: switchExpectedCaption }).waitFor({ state: "visible" });
  await page.locator(".lux-player-danmaku-text", { hasText: switchExpectedDanmaku }).waitFor({ state: "visible" });
  if (switchExpectedCaption !== expectedCaption && await page.locator(".lux-player-caption-text", { hasText: expectedCaption }).count()) {
    throw new Error("source switching left the previous caption visible");
  }
  if (switchExpectedDanmaku !== expectedDanmaku && await page.locator(".lux-player-danmaku-text", { hasText: expectedDanmaku }).count()) {
    throw new Error("source switching left the previous danmaku visible");
  }
  sourceSwitch = {
    requested: true,
    stoppedPreviousSession: sessionStopResponses.length > stopsBeforeSwitch,
    singleOverlay: await page.locator("[data-lux-danmaku-overlay]").count() === 1
      && await page.locator(".lux-player-caption-overlay").count() <= 1,
    selectedSourceId: await page.getByLabel("选择播放版本", { exact: true }).inputValue(),
  };
}

const activeExpectedCaption = switchSourceId ? switchExpectedCaption : expectedCaption;
const activeExpectedDanmaku = switchSourceId ? switchExpectedDanmaku : expectedDanmaku;
const activeExpectedChapter = switchSourceId ? switchExpectedChapter : expectedChapter;

if (stage18) {
  await video.evaluate((element) => {
    element.muted = true;
    return element.paused ? element.play() : undefined;
  });
  await page.waitForFunction(
    () => document.querySelector(".lux-player-page")?.classList.contains("controls-hidden"),
    undefined,
    { timeout: 5_000 },
  );
  const miniProgress = page.locator('[data-lux-player-mini-progress="true"]');
  await miniProgress.waitFor({ state: "attached" });
  const miniProgressState = await miniProgress.evaluate((element) => {
    const bounds = element.getBoundingClientRect();
    const played = element.querySelector(".lux-player-mini-progress-played");
    const buffered = element.querySelector(".lux-player-mini-progress-buffered");
    return {
      hiddenControls: element.closest(".lux-player-page")?.classList.contains("controls-hidden") ?? false,
      ariaHidden: element.getAttribute("aria-hidden") === "true",
      pointerEvents: getComputedStyle(element).pointerEvents,
      inViewport: bounds.left >= 0 && bounds.right <= window.innerWidth
        && bounds.top >= 0 && bounds.bottom <= window.innerHeight,
      playedWidth: played instanceof HTMLElement ? played.style.width : "",
      bufferedWidth: buffered instanceof HTMLElement ? buffered.style.width : "",
    };
  });
  await page.locator(".lux-player-page").hover();
  await page.waitForFunction(() => !document.querySelector(".lux-player-page")?.classList.contains("controls-hidden"));
  stage18MiniProgress = {
    checked: true,
    hidden: miniProgressState.hiddenControls && miniProgressState.inViewport,
    nonInteractive: miniProgressState.ariaHidden && miniProgressState.pointerEvents === "none",
    reflectsProgress: miniProgressState.playedWidth !== "0%" && miniProgressState.bufferedWidth !== "0%",
    removedOnActivity: await page.locator('[data-lux-player-mini-progress="true"]').count() === 0,
  };
}

const measureOverlayLayout = () => page.evaluate(() => {
  const topbar = document.querySelector(".lux-player-topbar")?.getBoundingClientRect();
  const controls = document.querySelector(".lux-player-controls-wrap")?.getBoundingClientRect();
  const caption = document.querySelector(".lux-player-caption-text")?.getBoundingClientRect();
  const danmaku = [...document.querySelectorAll(".lux-player-danmaku-text")]
    .map((element) => element.getBoundingClientRect());
  if (!topbar || !controls || !caption || danmaku.length === 0) {
    return { captionVisible: Boolean(caption), danmakuVisible: danmaku.length > 0, chromeSafe: false, layersSeparated: false };
  }
  const insideChrome = (bounds) => bounds.top >= topbar.bottom && bounds.bottom <= controls.top;
  const overlaps = (first, second) => (
    first.left < second.right
    && first.right > second.left
    && first.top < second.bottom
    && first.bottom > second.top
  );
  return {
    captionVisible: true,
    danmakuVisible: true,
    chromeSafe: insideChrome(caption) && danmaku.every(insideChrome),
    layersSeparated: danmaku.every((bounds) => !overlaps(caption, bounds)),
  };
});

const initialLayout = await measureOverlayLayout();
const safeZone = initialLayout.chromeSafe && initialLayout.layersSeparated;
if (!safeZone) throw new Error("caption or danmaku overlaps another player layer");

await timeline.focus();
const keyboardFocus = await timeline.evaluate((element) => {
  const bounds = element.getBoundingClientRect();
  return document.activeElement === element
    && bounds.left >= 0
    && bounds.right <= window.innerWidth
    && bounds.top >= 0
    && bounds.bottom <= window.innerHeight;
});
await timeline.press("Home");
await page.waitForFunction(() => (document.querySelector("video.lux-video")?.currentTime ?? 1) <= 0.25);
await timeline.press("ArrowRight");
await page.waitForFunction(() => (document.querySelector("video.lux-video")?.currentTime ?? 0) >= 1);
const keyboardSeek = await video.evaluate((element) => element.currentTime >= Math.min(1, element.duration));

await page.setViewportSize({ width: 390, height: 844 });
await page.locator(".lux-player-page").hover();
await timeline.focus();
await timeline.press("Home");
const touchTimelineBox = await timeline.boundingBox();
if (!touchTimelineBox) throw new Error("touch timeline is not measurable");
const cdp = await context.newCDPSession(page);
const touchY = touchTimelineBox.y + touchTimelineBox.height / 2;
await cdp.send("Input.dispatchTouchEvent", {
  type: "touchStart",
  touchPoints: [{ x: touchTimelineBox.x + touchTimelineBox.width * 0.2, y: touchY, id: 1 }],
});
await cdp.send("Input.dispatchTouchEvent", {
  type: "touchMove",
  touchPoints: [{ x: touchTimelineBox.x + touchTimelineBox.width * 0.7, y: touchY, id: 1 }],
});
await cdp.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
await page.waitForFunction(() => {
  const element = document.querySelector("video.lux-video");
  return Boolean(element && element.duration > 0 && element.currentTime >= element.duration * 0.5);
});
const touchSeek = true;

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
  element.currentTime = Math.min(1.5, Number.isFinite(element.duration) ? element.duration * 0.3 : 1.5);
  element.dispatchEvent(new Event("timeupdate"));
});
if (activeExpectedCaption) {
  await page.locator(".lux-player-caption-text", { hasText: activeExpectedCaption }).waitFor({ state: "visible" });
}
await page.locator(".lux-player-danmaku-text", { hasText: activeExpectedDanmaku }).waitFor({ state: "visible" });

const viewportChecks = [];
for (const viewport of [{ width: 390, height: 844 }, { width: 768, height: 1024 }, { width: 1440, height: 900 }]) {
  await page.setViewportSize(viewport);
  await page.locator(".lux-player-page").hover();
  await timeline.focus();
  const layout = await measureOverlayLayout();
  const controlsLayout = await page.locator(".lux-player-controls-wrap").evaluate((element) => {
    const bounds = element.getBoundingClientRect();
    const rightGroup = element.querySelector(".lux-player-controls-right");
    const rightStyle = rightGroup instanceof HTMLElement ? getComputedStyle(rightGroup) : null;
    return {
      inViewport: bounds.left >= 0 && bounds.right <= window.innerWidth
        && bounds.top >= 0 && bounds.bottom <= window.innerHeight,
      responsive: window.innerWidth !== 390 || Boolean(
        rightGroup instanceof HTMLElement
        && rightStyle?.overflowX === "auto"
        && rightGroup.clientWidth <= rightGroup.scrollWidth,
      ),
    };
  });
  if (artifactDir) {
    await page.screenshot({ path: join(artifactDir, `lux-player-${viewport.width}x${viewport.height}.png`) });
  }
  viewportChecks.push({
    ...viewport,
    noHorizontalOverflow: await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth),
    controlsInViewport: controlsLayout.inViewport,
    controlsResponsive: controlsLayout.responsive,
    controlsNamed: await page.getByRole("button", { name: "播放器设置", exact: true }).count() === 1
      && await page.getByRole("button", { name: "截图", exact: true }).count() === 1
      && await page.getByRole("button", { name: "画中画", exact: true }).count() === 1
      && await page.getByRole("button", { name: /全屏/ }).count() === 1,
    keyboardFocus: await timeline.evaluate((element) => document.activeElement === element),
    captionVisible: layout.captionVisible,
    danmakuVisible: layout.danmakuVisible,
    chromeSafe: layout.chromeSafe,
    layersSeparated: layout.layersSeparated,
    chapterVisible: !activeExpectedChapter
      || await page.getByRole("button", { name: `章节：${activeExpectedChapter}`, exact: true }).count() === 1,
  });
}

await Promise.all(playbackPlanReads);
const activeSessionId = playbackPlans.at(-1)?.sessionId;
if (!activeSessionId) throw new Error("active playback session was not observed");
const activeSessionPath = `/api/v1/playback/sessions/${encodeURIComponent(activeSessionId)}`;
const activeOverlayCounts = {
  caption: await page.locator(".lux-player-caption-overlay").count(),
  danmaku: await page.locator("[data-lux-danmaku-overlay]").count(),
  expectedDanmaku: await page.locator(".lux-player-danmaku-text", { hasText: activeExpectedDanmaku }).count(),
};
const finalMediaState = await video.evaluate((element) => ({
  currentSrc: element.currentSrc,
  duration: element.duration,
  currentTime: element.currentTime,
}));
const isDeclaredLuxRequest = ({ method, path }) => {
  if (method === "GET" && /^\/watch\/[^/]+$/.test(path)) return true;
  if (method === "GET" && (path.startsWith("/assets/") || path.startsWith("/hevc/") || path === "/favicon-white.svg")) return true;
  if (method === "GET" && ["/api/v1/auth/avatar", "/api/v1/auth/me", "/api/v1/setup/status"].includes(path)) return true;
  if (method === "GET" && /^\/api\/v1\/items\/[^/]+$/.test(path)) return true;
  if (method === "GET" && /^\/api\/v1\/items\/[^/]+\/(?:playback|danmaku)$/.test(path)) return true;
  if (method === "GET" && /^\/api\/v1\/items\/[^/]+\/danmaku\/raw$/.test(path)) return true;
  if (method === "GET" && /^\/api\/v1\/items\/[^/]+\/subtitles\/\d+$/.test(path)) return true;
  if (method === "POST" && path === "/api/v1/playback/sessions") return true;
  if (method === "DELETE" && /^\/api\/v1\/playback\/sessions\/[^/]+$/.test(path)) return true;
  if (method === "POST" && /^\/api\/v1\/playback\/sessions\/[^/]+\/(?:events|heartbeat)$/.test(path)) return true;
  if (method === "GET" && /^\/api\/v1\/playback\/sessions\/[^/]+\/(?:direct|hls\/[^/]+)$/.test(path)) return true;
  return false;
};
const unexpectedRequests = playerRequests.filter((request) => !isDeclaredLuxRequest(request));

if (await video.evaluate((element) => element.paused)) {
  await page.locator(".lux-player-page").hover();
  await page.locator(".lux-player-play-btn").click();
  await page.waitForFunction(() => document.querySelector("video.lux-video")?.paused === false);
}
capturePlayerRequests = false;
const leaveStopResponse = page.waitForResponse((response) => (
  (
    response.request().method() === "POST"
    && new URL(response.url()).pathname === `${activeSessionPath}/events`
    && response.request().postDataJSON()?.state === "STOPPED"
    && response.status() === 200
  ) || (
    response.request().method() === "DELETE"
    && new URL(response.url()).pathname === activeSessionPath
    && response.status() === 204
  )
), { timeout: 10_000 }).catch(() => null);
const backSteps = switchSourceId ? 2 : 1;
for (let index = 0; index < backSteps; index += 1) {
  const previousUrl = page.url();
  await page.locator(".lux-player-page").hover();
  await page.getByRole("button", { name: "返回", exact: true }).click();
  if (index + 1 < backSteps) {
    await page.waitForFunction((url) => window.location.href !== url, previousUrl);
  }
}
const stoppedOnLeave = Boolean(await leaveStopResponse);
await page.locator(".lux-player-page").waitFor({ state: "detached" });
const lifecycle = {
  stoppedOnLeave,
  overlaysCleared: await page.locator("[data-lux-danmaku-overlay], .lux-player-caption-overlay").count() === 0,
};

const result = {
  viewportChecks,
  danmakuResponses: danmakuResponses.map(({ method, path, sourceId: responseSourceId, status }) => ({
    method,
    path,
    sourceId: responseSourceId,
    status,
  })),
  playbackPlans,
  hlsResponses: hlsResponses.map(({ method, path, status }) => ({ method, path, status })),
  hevcResponses: hevcResponses.map(({ method, path, status }) => ({ method, path, status })),
  captionResponses: captionResponses.map(({ method, path, status }) => ({ method, path, status })),
  hasRawRead: danmakuResponses.some(({ path, status }) => path.endsWith("/danmaku/raw") && status === 200),
  hasVisibleDanmaku: activeOverlayCounts.expectedDanmaku > 0,
  activeOverlayCounts,
  safeZone,
  keyboardFocus,
  keyboardSeek,
  touchSeek,
  sourceSwitch,
  stage18Settings,
  stage18Chapters,
  stage18AirPlay,
  stage18MiniProgress,
  lifecycle,
  finalMediaState: {
    sourceKind: finalMediaState.currentSrc.startsWith("blob:") ? "blob" : "http",
    duration: finalMediaState.duration,
    currentTime: finalMediaState.currentTime,
  },
  declaredPlayerRequests: playerRequests.length,
  unexpectedRequests,
  sessionRequests,
  sessionResponses,
  sessionStopResponses,
  compatibilityDanmakuRequests,
  externalRequests,
  consoleErrors,
  consoleWarnings,
  pageErrors,
};
const finalPlan = playbackPlans.at(-1)?.type ?? null;
const hasExpectedPlan = forceHls
  ? finalPlan === "SERVER_HLS" && playbackPlans.some(({ type }) => type === "DIRECT")
  : finalPlan === "DIRECT" && playbackPlans.every(({ type }) => type === "DIRECT");
const hasHlsAsset = (suffix) => result.hlsResponses.some(({ path, status }) => path.endsWith(suffix) && status === 200);
result.hasExpectedPlan = hasExpectedPlan;
result.hasHlsAssets = {
  manifest: hasHlsAsset("/hls/index.m3u8"),
  init: hasHlsAsset("/hls/init.mp4"),
  segment: result.hlsResponses.some(({ path, status }) => /\/hls\/segment_[^/]+\.m4s$/.test(path) && status === 200),
};
result.hasClientHevcAssets = {
  worker: result.hevcResponses.some(({ path, status }) => path === "/hevc/transcode-worker.js" && status === 200),
  wasmGlue: result.hevcResponses.some(({ path, status }) => path === "/hevc/hevc-decode.js" && status === 200),
  wasmBinary: result.hevcResponses.some(({ path, status }) => path === "/hevc/hevc-decode.wasm" && status === 200),
  mseBlob: !forceClientHevc || finalMediaState.currentSrc.startsWith("blob:"),
};
result.hasCaption = !expectedCaption || result.captionResponses.some(({ status }) => status === 200);
console.log(JSON.stringify(result, null, 2));
await browser.close();

if (
  !result.hasRawRead
  || !result.hasVisibleDanmaku
  || !result.safeZone
  || !result.keyboardFocus
  || !result.keyboardSeek
  || !result.touchSeek
  || (result.sourceSwitch.requested && (
    !result.sourceSwitch.stoppedPreviousSession
    || !result.sourceSwitch.singleOverlay
    || result.sourceSwitch.selectedSourceId !== switchSourceId
  ))
  || !result.lifecycle.stoppedOnLeave
  || !result.lifecycle.overlaysCleared
  || result.unexpectedRequests.length > 0
  || result.compatibilityDanmakuRequests.length > 0
  || result.externalRequests.length > 0
  || result.consoleErrors.length > 0
  || result.consoleWarnings.length > 0
  || result.pageErrors.length > 0
  || !result.hasExpectedPlan
  || (forceHls && Object.values(result.hasHlsAssets).some((present) => !present))
  || (!forceHls && result.hlsResponses.length > 0)
  || (forceClientHevc && Object.values(result.hasClientHevcAssets).some((present) => !present))
  || !result.hasCaption
  || result.viewportChecks.some((check) => (
    !check.noHorizontalOverflow
    || !check.controlsInViewport
    || !check.controlsResponsive
    || !check.controlsNamed
    || !check.keyboardFocus
    || !check.captionVisible
    || !check.danmakuVisible
    || !check.chromeSafe
    || !check.layersSeparated
    || !check.chapterVisible
  ))
  || (stage18 && (
    !result.stage18Settings.checked
    || !result.stage18Settings.sessionCreatesUnchanged
    || !result.stage18Settings.presentationApplied
    || !result.stage18Settings.offsetApplied
    || !result.stage18Chapters.checked
    || !result.stage18Chapters.chapterSeek
    || !result.stage18Chapters.introSkip
    || !result.stage18Chapters.creditsVisible
    || (result.stage18AirPlay.mode === "available" && (
      !result.stage18AirPlay.availableShown
      || !result.stage18AirPlay.pickerCalled
    ))
    || !result.stage18AirPlay.unavailableHidden
    || !result.stage18AirPlay.sessionCreatesUnchanged
    || !result.stage18MiniProgress.checked
    || !result.stage18MiniProgress.hidden
    || !result.stage18MiniProgress.nonInteractive
    || !result.stage18MiniProgress.reflectsProgress
    || !result.stage18MiniProgress.removedOnActivity
  ))
) process.exitCode = 1;
