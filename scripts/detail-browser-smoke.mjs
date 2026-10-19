// Run after `pnpm --dir web build` and
// `pnpm --dir web exec vite preview --host 127.0.0.1 --port 5199 --strictPort`:
// playwright-cli -s=lux-detail open about:blank
// playwright-cli -s=lux-detail run-code --filename scripts/detail-browser-smoke.mjs
// All API responses and artwork below are synthetic; no server or user data is used.
async (page) => {
  const base = "http://127.0.0.1:5199";
  const check = (value, message) => { if (!value) throw new Error(message); };
  const errors = [];
  const unexpected = [];
  const editorRequests = [];
  const mutations = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => { if (["error", "warning"].includes(message.type())) errors.push(message.text()); });
  page.on("request", (request) => {
    const name = new URL(request.url()).pathname.split("/").pop();
    if (/^Media(MetadataEditor|ImageEditor|SubtitleEditor|Identifier|DeleteDialog)-.*\.js$/.test(name)) editorRequests.push(name);
  });
  const streams = [
    { index: 0, type: "VIDEO", codec: "h264", width: 1920, height: 1080 },
    { index: 1, type: "AUDIO", codec: "aac", language: "eng", isDefault: true },
    { index: 2, type: "AUDIO", codec: "aac", language: "zho", title: "国语" },
    { index: 3, type: "SUBTITLE", codec: "srt", language: "zho", title: "简体中文", isExternal: true, isDefault: true },
  ];
  const source = (id, isDefault = true) => ({ id, isDefault, sourceKind: "LOCAL_FILE", container: "mp4", editionName: id === "source-1" ? "1080p" : "4K", streams });
  const movie = {
    id: "movie-1", libraryId: "library-1", itemType: "MOVIE", title: "详情性能验证", originalTitle: "Detail Preview",
    productionYear: 2026, overview: "使用合成媒体验证页面拆分、版本选择和编辑器按需加载。", rating: 8.2,
    imageTags: { poster: "poster", fanart: "fanart" }, nfo: { tagline: "流畅浏览每一帧", studios: ["测试工作室"], genres: ["剧情"] },
    actors: Array.from({ length: 20 }, (_, index) => ({ id: `actor-${index}`, name: `演员 ${index + 1}`, role: "测试角色" })),
    mediaSources: [source("source-1"), source("source-2", false)],
  };
  const series = { ...movie, id: "series-1", title: "合成剧集", itemType: "SERIES", mediaSources: [], seasonCount: 1, episodeCount: 2 };
  const season = { id: "season-1", title: "第一季", itemType: "SEASON", parentId: series.id, seriesId: series.id, parentIndexNumber: 1, mediaSources: [] };
  const episodes = [1, 2].map((index) => ({
    ...movie, id: `episode-${index}`, title: `第${index}集`, itemType: "EPISODE", parentId: season.id, seriesId: series.id,
    indexNumber: index, parentIndexNumber: 1, mediaSources: [source(`episode-source-${index}`)], userData: { isPlayed: index === 1 },
  }));
  const items = Object.fromEntries([movie, series, season, ...episodes].map((item) => [item.id, item]));
  let favorite = false;
  let played = false;
  await page.context().route(`${base}/api/**`, async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const path = url.pathname;
    const json = (body) => route.fulfill({ json: body });
    if (path.endsWith("/avatar") || /\/images\/[^/]+$/.test(path)) return route.fulfill({
      contentType: "image/svg+xml", body: '<svg xmlns="http://www.w3.org/2000/svg" width="400" height="600"><rect width="400" height="600" fill="#284e57"/><text x="35" y="300" fill="white" font-size="32">LUX PREVIEW</text></svg>',
    });
    if (path === "/api/v1/setup/status") return json({ initialized: true });
    if (path === "/api/v1/auth/me") return json({ user: { id: "preview-user", displayName: "Preview", canManageServer: false } });
    if (path === "/api/v1/events") return route.fulfill({ status: 204 });
    if (path.endsWith("/identify/candidates")) return json({ items: [], total: 0 });
    if (path.endsWith("/images")) return json({ images: [{
      id: "still-1", itemId: movie.id, imageType: "FANART", imageIndex: 0, url: `${base}/api/v1/items/movie-1/images/still-1`,
    }] });
    if (path.endsWith("/children")) {
      const isSeasons = url.searchParams.get("itemType") === "SEASON";
      const currentPage = Number(url.searchParams.get("page") || 1);
      return json({ items: isSeasons ? [season] : [episodes[currentPage - 1]].filter(Boolean), total: isSeasons ? 1 : 2, page: currentPage, pageSize: 1 });
    }
    if (path.endsWith("/playback")) return json({ isFavorite: favorite, isPlayed: played });
    if (path.endsWith("/favorite") || path.endsWith("/played")) {
      const body = request.postDataJSON();
      if (path.endsWith("/favorite")) favorite = body.favorite;
      else played = body.played;
      mutations.push({ method: request.method(), path });
      return route.fulfill({ status: 204 });
    }
    const id = path.split("/")[4];
    if (path.endsWith("/metadata")) {
      if (request.method() === "PATCH") {
        Object.assign(items[id], request.postDataJSON());
        mutations.push({ method: request.method(), path });
      }
      return json({ title: items[id]?.title, overview: items[id]?.overview, lockedFields: [] });
    }
    if (path.startsWith("/api/v1/admin/items/") && request.method() === "DELETE") {
      mutations.push({ method: "DELETE", path, selectedSource: url.searchParams.get("sourceId") });
      return route.fulfill({ status: 409, json: { message: "合成删除失败，请重试。" } });
    }
    if (items[id]) return json(items[id]);
    unexpected.push(`${request.method()} ${path}`);
    return route.fulfill({ status: 404, json: { message: "Unexpected synthetic fixture request" } });
  });
  await page.goto(`${base}/items/movie-1`);
  await page.getByRole("heading", { name: movie.title, exact: true }).waitFor();
  await page.locator(".lux-detail-stills img").waitFor();
  check(editorRequests.length === 0, "Editors were downloaded before opening an action");
  await page.getByRole("combobox", { name: "选择版本" }).click();
  await page.getByRole("option").filter({ hasText: "4K" }).click();
  check((await page.locator(".lux-hero-actions a").getAttribute("href")).includes("sourceId=source-2"), "Selected source missing from play link");
  await page.getByRole("combobox", { name: "选择音频" }).click();
  await page.getByRole("option").filter({ hasText: "国语" }).click();
  check((await page.locator(".lux-hero-actions a").getAttribute("href")).includes("audioStreamIndex=2"), "Selected audio missing from play link");
  await page.locator("[data-action=toggle-favorite]").click();
  await page.waitForFunction(() => document.querySelector("[data-action=toggle-favorite]")?.getAttribute("aria-pressed") === "true");
  await page.locator("[data-action=toggle-played]").click();
  await page.waitForFunction(() => document.querySelector("[data-action=toggle-played]")?.getAttribute("aria-pressed") === "true");
  const editorActions = [
    ["edit-metadata", ".lux-metadata-editor", "MediaMetadataEditor", "关闭元数据编辑"],
    ["edit-images", ".lux-image-editor", "MediaImageEditor", "关闭图像编辑"],
    ["edit-subtitles", ".lux-subtitle-editor", "MediaSubtitleEditor", "关闭字幕编辑"],
    ["identify", ".lux-identifier-editor", "MediaIdentifier", "关闭元数据匹配"],
    ["delete", ".lux-delete-dialog", "MediaDeleteDialog", "关闭删除确认"],
  ];
  for (const [action, selector, chunk, close] of editorActions) {
    check(!editorRequests.some((name) => name.startsWith(`${chunk}-`)), `${chunk} loaded too early`);
    await page.locator(".lux-detail-inline-menu .lux-media-actions-trigger").click();
    await page.locator(`[data-action=${action}]`).click();
    await page.locator(selector).waitFor();
    check(editorRequests.some((name) => name.startsWith(`${chunk}-`)), `${chunk} did not load on demand`);
    if (action === "edit-metadata") {
      await page.getByLabel("标题", { exact: true }).fill("更新后的合成标题");
      await page.getByRole("button", { name: "保存", exact: true }).click();
      await page.locator(selector).waitFor({ state: "detached" });
    } else {
      if (action === "delete") {
        await page.locator("[data-action=delete-confirm]").click();
        await page.locator(`${selector} [role=alert]`).waitFor();
      }
      await page.getByRole("button", { name: close, exact: true }).click();
    }
    check(await page.locator(selector).count() === 0, `${chunk} remained mounted after close`);
  }
  check(mutations.some((entry) => entry.method === "DELETE" && entry.selectedSource === "source-2"), "Delete lost the selected version");
  const viewportChecks = [];
  for (const width of [1440, 390]) {
    await page.setViewportSize({ width, height: 900 });
    const noOverflow = await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth);
    check(noOverflow, `Horizontal overflow at ${width}px`);
    viewportChecks.push({ width, noOverflow });
    await page.screenshot({ path: `output/playwright/detail-${width}.png`, fullPage: true });
  }
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto(`${base}/items/series-1`);
  await page.locator(".lux-season-episode-row").waitFor();
  check(await page.locator(".lux-season-episode-row").count() === 1, "Single season did not expand");
  await page.locator("[data-action=load-more-episodes]").click();
  await page.waitForFunction(() => document.querySelectorAll(".lux-season-episode-row").length === 2);
  check((await page.locator(".lux-hero-actions a").getAttribute("href")) === "/watch/episode-2", "Next playable episode is incorrect");
  await page.goto(`${base}/items/season-1`);
  await page.locator(".lux-detail-page-season").waitFor();
  check((await page.locator(".lux-detail-poster img").getAttribute("src")).includes("/series-1/"), "Season poster fallback was lost");
  check(unexpected.length === 0, `Unexpected API requests: ${unexpected.join(", ")}`);
  const expectedHttpErrors = errors.filter((message) => message.includes("server responded with a status of 409"));
  const unexpectedErrors = errors.filter((message) => !expectedHttpErrors.includes(message));
  check(expectedHttpErrors.length === 1, "Expected one synthetic deletion failure");
  check(unexpectedErrors.length === 0, `Browser errors: ${unexpectedErrors.join(", ")}`);
  return { browser: await page.context().browser().version(), editorsDownloadedOnInitialDetail: 0, editorChunksLoadedOnDemand: editorRequests, viewportChecks, mutations, singleSeasonAndPagination: true, nextEpisode: "episode-2", seasonPosterFallback: true, unexpectedErrors, expectedDeletionHttpErrors: expectedHttpErrors.length, unexpected };
}
