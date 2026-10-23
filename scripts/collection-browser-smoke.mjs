// Run against `pnpm --dir web exec vite preview --host 127.0.0.1 --port 5199 --strictPort`:
// playwright-cli --session lux-collection run-code --filename scripts/collection-browser-smoke.mjs
// API responses and artwork are synthetic; no server or user data is used.
async (page) => {
  const base = "http://127.0.0.1:5199";
  const check = (condition, message) => { if (!condition) throw new Error(message); };
  const errors = [];
  const unexpected = [];
  const saves = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => {
    if (["error", "warning"].includes(message.type())) errors.push(message.text());
  });
  const collection = {
    id: "collection-preview", title: "飞驰人生（系列）", originalTitle: "Pegasus Collection", itemType: "BOX_SET",
    productionYear: 2019, imageTags: { poster: "poster", fanart: "fanart" },
    overview: "曾经叱咤赛场的赛车手再次集结，与朋友们踏上重返赛道的旅程。从第一次出发到再度追逐冠军，热爱让他们一次次突破极限，也让每一段旅程都有了新的意义。",
  };
  const copy = (number, suffix = "") => ({
    itemId: `movie-${number}${suffix}`, libraryId: `library${suffix}`, libraryName: suffix ? "收藏电影" : "客厅电影",
    title: `飞驰人生 ${number}`, posterImageTag: `poster-${number}`,
    versions: [{ id: `version-${number}${suffix}`, qualityLabel: suffix ? "2160p" : "1080p", container: "mkv" }],
  });
  const members = Array.from({ length: 9 }, (_, index) => ({
    id: `work-${index + 1}`, title: index === 0 ? "飞驰人生" : `飞驰人生 ${index + 1}`, itemType: "MOVIE",
    productionYear: 2019 + index, sources: index === 0 ? [copy(1), copy(1, "-4k")] : [copy(index + 1)],
  }));
  members[1].sources[0].versions.push({ id: "version-2-alt", qualityLabel: "2160p", container: "mkv" });
  members[1].preferredSourceItemId = "movie-2";
  members[1].preferredMediaSourceId = "version-2-alt";
  await page.context().route(`${base}/api/**`, async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const path = url.pathname;
    const json = (body) => route.fulfill({ json: body });
    if (path === "/api/v1/setup/status") return json({ initialized: true });
    if (path === "/api/v1/auth/me") return json({ user: { id: "preview", displayName: "Preview", canManageServer: false } });
    if (path === "/api/v1/events") return route.fulfill({ status: 204 });
    if (path.endsWith("/avatar") || /\/images\/[^/]+$/.test(path)) {
      const number = Number(path.match(/movie-(\d+)/)?.[1] ?? 1);
      const colors = ["#194954", "#6f3627", "#303a58", "#675431", "#254c44"];
      return route.fulfill({ contentType: "image/svg+xml", body: `<svg xmlns="http://www.w3.org/2000/svg" width="600" height="900" viewBox="0 0 600 900"><defs><linearGradient id="sky" x2="0" y2="1"><stop stop-color="${colors[(number - 1) % colors.length]}"/><stop offset="1" stop-color="#101820"/></linearGradient></defs><rect width="600" height="900" fill="url(#sky)"/><circle cx="440" cy="220" r="170" fill="#f5dba5" opacity=".25"/><path d="M0 620L600 400V900H0" fill="#151e27"/><path d="M70 900L440 510L550 510L380 900" fill="#e4d9b3" opacity=".35"/><path d="M320 610h110l25 35H300z" fill="#ec9c52"/><text x="44" y="165" fill="#f6e7ca" font-size="66" font-family="serif" font-weight="bold">飞驰人生</text><text x="48" y="220" fill="#f6e7ca" font-size="25" letter-spacing="8">PEGASUS · ${number}</text></svg>` });
    }
    if (path.endsWith("/source") && request.method() === "PUT") {
      const selection = request.postDataJSON();
      saves.push(selection);
      members[0].preferredSourceItemId = selection.sourceItemId;
      members[0].preferredMediaSourceId = selection.mediaSourceId;
      return route.fulfill({ status: 204 });
    }
    if (path === "/api/v1/collections/collection-preview") {
      const secondPage = url.searchParams.get("page") === "2";
      return json({ collection, items: secondPage ? members.slice(8) : members.slice(0, 8), page: secondPage ? 2 : 1, pageSize: 8, total: 9 });
    }
    if (path.endsWith("/images")) return json({ images: [] });
    if (path.endsWith("/playback")) return json({ isFavorite: false, isPlayed: false });
    const movie = path.match(/^\/api\/v1\/items\/(movie-(\d+)(?:-4k)?)$/);
    if (movie) return json({ ...members[Number(movie[2]) - 1], id: movie[1], mediaSources: [] });
    unexpected.push(`${request.method()} ${path}`);
    return route.fulfill({ status: 404, json: { message: "Unexpected fixture request" } });
  });

  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.goto(`${base}/collections/${collection.id}`);
  await page.getByRole("heading", { name: collection.title, exact: true }).waitFor();
  await page.getByRole("button", { name: "向右滚动合集影片", exact: true }).waitFor();
  check(await page.locator(".lux-media-card").count() === 8, "Initial collection page is missing cards");
  check(await page.locator(".lux-collection-work select, .lux-collection-source-control").count() === 0, "Source controls are still visible under the cards");
  await page.getByRole("button", { name: "加载更多影片", exact: true }).click();
  await page.waitForFunction(() => document.querySelectorAll(".lux-media-card").length === 9);
  for (const theme of ["dark", "light"]) {
    await page.evaluate((value) => document.documentElement.dataset.luxTheme = value, theme);
    for (const width of [320, 768, 1024, 1440]) {
      await page.setViewportSize({ width, height: 1000 });
      await page.locator(".lux-collection-members").scrollIntoViewIfNeeded();
      const metrics = await page.evaluate(() => {
        const cards = [...document.querySelectorAll(".lux-collection-work")].slice(0, 2).map((card) => card.getBoundingClientRect());
        const overview = document.querySelector(".lux-detail-overview");
        const rail = document.querySelector(".lux-horizontal-scroll-viewport");
        return {
          pageWidth: document.documentElement.scrollWidth, viewport: innerWidth,
          horizontal: Math.abs(cards[0].top - cards[1].top) < 1 && cards[1].left > cards[0].left,
          overviewVisible: getComputedStyle(overview).display !== "none" && overview.getBoundingClientRect().height > 0,
          overflow: rail.scrollWidth > rail.clientWidth,
        };
      });
      check(metrics.pageWidth <= metrics.viewport + 1, `${theme} ${width}px: page overflows horizontally`);
      check(metrics.horizontal && metrics.overflow, `${theme} ${width}px: movie cards are not a horizontal rail`);
      check(metrics.overviewVisible, `${theme} ${width}px: collection overview is hidden`);
      if (width === 1440) {
        await page.evaluate(() => window.scrollTo(0, 0));
        await page.mouse.move(0, 0);
        await page.screenshot({ path: `output/playwright/collection-${theme}-desktop.png`, fullPage: true, animations: "disabled" });
      }
      if (width === 320) {
        await page.evaluate(() => window.scrollTo(0, 0));
        await page.screenshot({ path: `output/playwright/collection-${theme}-mobile.png`, fullPage: true, animations: "disabled" });
      }
    }
  }
  await page.getByRole("button", { name: "向右滚动合集影片", exact: true }).click();
  await page.waitForFunction(() => document.querySelector(".lux-horizontal-scroll-viewport").scrollLeft > 0);
  await page.getByRole("button", { name: "向左滚动合集影片", exact: true }).click();
  await page.waitForFunction(() => document.querySelector(".lux-horizontal-scroll-viewport").scrollLeft < 1);

  const firstCard = page.locator(".lux-collection-work").first();
  const posterButton = firstCard.locator(".lux-media-card-link").first();
  await posterButton.focus();
  await page.keyboard.press("Enter");
  const dialog = page.getByRole("dialog", { name: "选择影片来源" });
  await dialog.waitFor();
  check(await dialog.locator(".lux-collection-source-option").count() === 2, "Dialog is missing source choices");
  check(new URL(page.url()).pathname.startsWith("/collections/"), "Multiple sources navigated before selection");
  check(await dialog.evaluate((element) => element.matches(":modal") && element.contains(document.activeElement)), "Dialog did not receive modal keyboard focus");
  await page.keyboard.press("Tab");
  await page.keyboard.press("Tab");
  await page.keyboard.press("Shift+Tab");
  check(await dialog.evaluate((element) => element.contains(document.activeElement)), "Keyboard navigation left the source choices");
  await page.keyboard.press("Escape");
  await dialog.waitFor({ state: "hidden" });
  check(await posterButton.evaluate((button) => button === document.activeElement), "Escape did not restore focus to the poster");
  check(saves.length === 0, "Cancelling unexpectedly saved a source");
  await firstCard.locator(".lux-media-card-link").nth(1).click();
  await dialog.waitFor();
  for (const theme of ["dark", "light"]) {
    await page.evaluate((value) => document.documentElement.dataset.luxTheme = value, theme);
    for (const width of [320, 1440]) {
      await page.setViewportSize({ width, height: 1000 });
      const fits = await dialog.evaluate((element) => {
        const bounds = element.getBoundingClientRect();
        return bounds.left >= 0 && bounds.right <= innerWidth && bounds.top >= 0 && bounds.bottom <= innerHeight;
      });
      check(fits, `${theme} ${width}px: source dialog is outside the viewport`);
      await page.screenshot({ path: `output/playwright/collection-source-${theme}-${width === 320 ? "mobile" : "desktop"}.png`, animations: "disabled" });
    }
  }
  await page.mouse.click(4, 4);
  await dialog.waitFor({ state: "hidden" });
  await posterButton.click();
  await dialog.waitFor();
  await dialog.locator(".lux-collection-source-option").nth(1).click();
  await page.waitForURL("**/items/movie-1-4k?collectionId=collection-preview&sourceId=version-1-4k");
  check(saves.length === 1 && saves[0].mediaSourceId === "version-1-4k", "Source preference did not persist");
  await page.getByRole("link", { name: "返回合集", exact: true }).click();
  await page.getByRole("heading", { name: collection.title, exact: true }).waitFor();
  await page.locator(".lux-collection-work").nth(1).locator("a").first().click();
  await page.waitForURL("**/items/movie-2?collectionId=collection-preview&sourceId=version-2-alt");
  check(await page.locator("dialog").count() === 0 && saves.length === 1, "Single source with multiple versions did not enter directly");
  await page.getByRole("link", { name: "返回合集", exact: true }).click();
  await page.getByRole("heading", { name: collection.title, exact: true }).waitFor();
  check(unexpected.length === 0, `Unexpected requests: ${unexpected.join("; ")}`);
  check(errors.length === 0, `Browser errors: ${errors.join("; ")}`);
  console.log(JSON.stringify({ result: "passed", viewports: [320, 768, 1024, 1440], themes: ["dark", "light"], sourceSelection: saves, errors, unexpected }));
}
