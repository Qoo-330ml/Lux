// Run with the same playwright-cli session after detail-browser-smoke.mjs.
// The fixture routes remain active; this page delays and then rejects an editor chunk.
async (page) => {
  const failurePage = await page.context().newPage();
  const pageErrors = [];
  failurePage.on("pageerror", (error) => pageErrors.push(error.message));
  let release;
  const gate = new Promise((resolve) => { release = resolve; });
  await failurePage.route("**/assets/MediaMetadataEditor-*.js", async (route) => {
    await gate;
    await route.abort("failed");
  });
  try {
    await failurePage.goto("http://127.0.0.1:5199/items/movie-1");
    await failurePage.locator(".lux-detail-title-row h1").waitFor();
    await failurePage.locator(".lux-detail-inline-menu .lux-media-actions-trigger").click();
    await failurePage.locator("[data-action=edit-metadata]").click();
    await failurePage.getByRole("status").filter({ hasText: "正在加载编辑器" }).waitFor();
    const detailVisibleWhileLoading = await failurePage.locator(".lux-detail-title-row h1").isVisible();
    release();
    await failurePage.getByRole("alert").filter({ hasText: "编辑器加载失败" }).waitFor();
    const detailVisibleAfterFailure = await failurePage.locator(".lux-detail-title-row h1").isVisible();
    await failurePage.getByRole("button", { name: "关闭编辑器", exact: true }).click();
    const errorDismissed = await failurePage.getByRole("dialog").count() === 0;
    if (!detailVisibleWhileLoading || !detailVisibleAfterFailure || !errorDismissed || pageErrors.length) {
      throw new Error("Editor loading or failure replaced the detail page");
    }
    return { detailVisibleWhileLoading, detailVisibleAfterFailure, errorDismissed, pageErrors };
  } finally {
    release();
    await failurePage.close();
  }
}
