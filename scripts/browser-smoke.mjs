import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const { chromium } = require("playwright");

const baseUrl = process.env.LUX_E2E_BASE_URL || "http://127.0.0.1:8097";
const username = process.env.LUX_E2E_USERNAME || "viewer";
const password = process.env.LUX_E2E_PASSWORD;
const libraryName = process.env.LUX_E2E_LIBRARY || "";
const itemId = process.env.LUX_E2E_ITEM || "";
if (!password) throw new Error("LUX_E2E_PASSWORD is required");

const browser = await chromium.launch({
  headless: true,
  executablePath: process.env.PW_BROWSER || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
});
const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
const consoleErrors = [];
const pageErrors = [];
const failedRequests = [];
const streamResponses = [];
page.on("console", (message) => { if (message.type() === "error") consoleErrors.push(message.text()); });
page.on("pageerror", (error) => pageErrors.push(error.message));
page.on("requestfailed", (request) => failedRequests.push({ url: `${request.method()} ${request.url()}`, error: request.failure()?.errorText || "unknown" }));
page.on("response", (response) => { if (response.url().includes("/stream?")) streamResponses.push({ status: response.status(), url: response.url() }); });

await page.goto(`${baseUrl}/`, { waitUntil: "networkidle" });
await page.getByLabel("用户名", { exact: true }).fill(username);
await page.getByLabel("密码", { exact: true }).fill(password);
await Promise.all([
  page.waitForResponse((response) => response.url().endsWith("/api/v1/auth/login") && response.status() === 200),
  page.getByRole("button", { name: "登录", exact: true }).click(),
]);
await page.locator("main.content").waitFor();

const ordinaryUserNoAdmin = await page.locator("[data-route='admin']").count() === 0;
if (libraryName) {
  await page.locator("[data-library]").filter({ hasText: libraryName }).first().click();
} else if (await page.locator("[data-library]").count()) {
  await page.locator("[data-library]").first().click();
}
await page.locator("[data-item]").first().waitFor({ state: "visible" });
const item = itemId ? page.locator(`[data-item="${itemId}"]`) : page.locator("[data-item]").first();
await item.click();
await page.locator("[data-player]").waitFor({ state: "visible" });
await page.waitForFunction(() => document.querySelector("[data-player]")?.readyState >= 1);
const playerMetadataReady = await page.locator("[data-player]").evaluate((player) => player.readyState >= 1);

const favoriteResponse = page.waitForResponse((response) => response.url().includes("/favorite") && response.request().method() === "PUT");
await page.getByRole("button", { name: /收藏/ }).click();
const favoriteStatus = (await favoriteResponse).status();
await page.locator("[data-action='toggle-favorite']").waitFor();

const viewportChecks = [];
for (const viewport of [{ width: 390, height: 844 }, { width: 768, height: 1024 }, { width: 1440, height: 1000 }]) {
  await page.setViewportSize(viewport);
  viewportChecks.push({ ...viewport, noHorizontalOverflow: await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth) });
}

const accessibilityIssues = await page.evaluate(() => {
  const issues = [];
  if (!document.querySelector("main")) issues.push("missing main landmark");
  document.querySelectorAll("img").forEach((image) => { if (!image.getAttribute("alt")) issues.push("image without alt"); });
  document.querySelectorAll("input, select, textarea").forEach((control) => {
    const label = control.getAttribute("aria-label") || control.getAttribute("aria-labelledby") || (control.id && document.querySelector(`label[for='${CSS.escape(control.id)}']`));
    if (!label && !control.closest("label")) issues.push(`form control without label: ${control.name || control.type || control.tagName}`);
  });
  document.querySelectorAll("button, a").forEach((control) => {
    if (!(control.getAttribute("aria-label") || control.textContent.trim())) issues.push("interactive control without accessible name");
  });
  return issues;
});

const result = {
  ordinaryUserNoAdmin,
  playerMetadataReady,
  streamResponses,
  favoriteStatus,
  favoriteUpdated: [200, 204].includes(favoriteStatus),
  viewportChecks,
  accessibilityIssues,
  consoleErrors,
  pageErrors,
  failedRequests: failedRequests.filter((request) => !(request.url.includes("/favorite") && favoriteStatus === 204)),
};
console.log(JSON.stringify(result, null, 2));
await browser.close();
if (!ordinaryUserNoAdmin || !playerMetadataReady || streamResponses.every((response) => response.status !== 206) || ![200, 204].includes(favoriteStatus) || viewportChecks.some((check) => !check.noHorizontalOverflow) || accessibilityIssues.length || consoleErrors.length || pageErrors.length || result.failedRequests.length) process.exitCode = 1;
