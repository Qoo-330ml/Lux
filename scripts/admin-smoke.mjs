import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const { chromium } = require("playwright");
const baseUrl = process.env.LUX_E2E_BASE_URL || "http://127.0.0.1:8097";
const username = process.env.LUX_E2E_ADMIN_USERNAME || "admin";
const password = process.env.LUX_E2E_ADMIN_PASSWORD;
const rootPath = process.env.LUX_E2E_MEDIA_ROOT || "";
if (!password) throw new Error("LUX_E2E_ADMIN_PASSWORD is required");

const browser = await chromium.launch({
  headless: true,
  executablePath: process.env.PW_BROWSER || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
});
const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
const consoleErrors = [];
const pageErrors = [];
page.on("console", (message) => { if (message.type() === "error") consoleErrors.push(message.text()); });
page.on("pageerror", (error) => pageErrors.push(error.message));

await page.goto(`${baseUrl}/`, { waitUntil: "networkidle" });
await page.getByLabel("用户名", { exact: true }).fill(username);
await page.getByLabel("密码", { exact: true }).fill(password);
await Promise.all([
  page.waitForResponse((response) => response.url().endsWith("/api/v1/auth/login") && response.status() === 200),
  page.getByRole("button", { name: "登录", exact: true }).click(),
]);
await page.getByRole("button", { name: "管理", exact: true }).click();
await page.getByRole("heading", { name: "管理控制台", exact: true }).waitFor();

const readyResponse = page.waitForResponse((response) => response.url().endsWith("/health/ready") && response.status() === 200);
await page.getByRole("button", { name: "管理", exact: true }).click();
await readyResponse;

const libraryName = `Browser Admin ${Date.now()}`;
const createLibraryResponse = page.waitForResponse((response) => response.url().endsWith("/api/v1/admin/libraries") && response.request().method() === "POST" && response.status() === 201);
await page.getByLabel("媒体库名称", { exact: true }).fill(libraryName);
await page.getByRole("button", { name: "创建媒体库", exact: true }).click();
const libraryBody = await (await createLibraryResponse).json();
const libraryId = libraryBody.library.id;
await page.getByRole("heading", { name: libraryName, exact: true }).waitFor();

let rootDeleted = false;
if (rootPath) {
  const rootForm = page.locator(`form[data-action='add-root'][data-library-id='${libraryId}']`);
  await rootForm.getByLabel("根路径", { exact: true }).fill(rootPath);
  const rootResponse = page.waitForResponse((response) => response.url().includes(`/api/v1/admin/libraries/${libraryId}/roots`) && response.request().method() === "POST" && response.status() === 201);
  await rootForm.getByRole("button", { name: "添加根路径", exact: true }).click();
  await rootResponse;
  await page.locator(`[data-delete-root][data-library-id='${libraryId}']`).waitFor();
  const scanResponse = page.waitForResponse((response) => response.url().endsWith(`/api/v1/admin/libraries/${libraryId}/scan`) && response.status() === 202);
  await page.locator(`[data-scan-library='${libraryId}']`).click();
  await scanResponse;
  const jobsRefresh = page.waitForResponse((response) => response.url().includes("/api/v1/admin/jobs?") && response.status() === 200);
  await page.getByRole("button", { name: "筛选任务", exact: true }).click();
  await jobsRefresh;
  await page.getByRole("button", { name: "详情", exact: true }).first().click();
  await page.getByRole("heading", { name: "任务详情", exact: true }).waitFor();
  page.once("dialog", (dialog) => dialog.accept());
  const deleteRootResponse = page.waitForResponse((response) => response.url().includes(`/api/v1/admin/libraries/${libraryId}/roots/`) && response.request().method() === "DELETE" && response.status() === 204);
  await page.locator(`[data-delete-root][data-library-id='${libraryId}']`).click();
  await deleteRootResponse;
  rootDeleted = true;
}

const toggleForm = page.locator(`form[data-action='update-library'][data-library-id='${libraryId}']`);
const libraryCard = page.locator(".admin-library").filter({ hasText: libraryName });
await toggleForm.getByLabel("启用媒体库", { exact: true }).uncheck();
const disableResponse = page.waitForResponse((response) => response.url().endsWith(`/api/v1/admin/libraries/${libraryId}`) && response.request().method() === "PATCH" && response.status() === 200);
await toggleForm.getByRole("button", { name: "保存计划", exact: true }).click();
await disableResponse;
await page.getByText("已停用", { exact: false }).first().waitFor();

page.once("dialog", (dialog) => dialog.accept());
const deleteLibraryResponse = page.waitForResponse((response) => response.url().endsWith(`/api/v1/admin/libraries/${libraryId}`) && response.request().method() === "DELETE" && response.status() === 204);
await libraryCard.getByRole("button", { name: "删除媒体库", exact: true }).click();
await deleteLibraryResponse;

const result = { adminVisible: true, libraryCreated: true, rootDeleted, jobDetailVisible: Boolean(rootPath), libraryDisabled: true, libraryDeleted: true, consoleErrors, pageErrors };
console.log(JSON.stringify(result, null, 2));
await browser.close();
if (consoleErrors.length || pageErrors.length) process.exitCode = 1;
