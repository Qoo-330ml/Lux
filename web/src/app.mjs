import { requestOptions } from "./request-options.mjs";

const app = document.querySelector("#app");
const state = { user: null, initialized: true, libraries: [], home: null, admin: null, route: "home", libraryId: "", libraryFilters: {}, item: null, itemImages: [], itemCandidates: [], playback: null, children: null, error: "", notice: "", setupNotice: "" };

const api = {
  async request(path, options = {}) {
    const response = await fetch(path, requestOptions(options));
    if (response.status === 204) return null;
    const body = await response.json().catch(() => ({}));
    if (!response.ok) throw new Error(body?.error?.message || "请求失败");
    return body;
  },
  login(username, password) { return this.request("/api/v1/auth/login", { method: "POST", body: JSON.stringify({ username, password }) }); },
  setup(data) { return this.request("/api/v1/setup/complete", { method: "POST", body: JSON.stringify(data) }); },
  logout() { return this.request("/api/v1/auth/logout", { method: "POST", headers: { "x-csrf-token": readCookie("lux_csrf") } }); },
  setupStatus() { return this.request("/api/v1/setup/status"); },
  me() { return this.request("/api/v1/auth/me"); },
  sessions() { return this.request("/api/v1/auth/sessions"); },
  revokeSession(id) { return this.request("/api/v1/auth/sessions/" + encodeURIComponent(id), { method: "DELETE", headers: { "x-csrf-token": readCookie("lux_csrf") } }); },
  home() { return this.request("/api/v1/home"); },
  libraries() { return this.request("/api/v1/libraries"); },
  libraryItems(id, filters = {}) { const params = new URLSearchParams({ page: "1", pageSize: "60" }); Object.entries(filters).forEach(([key, value]) => { if (value !== "" && value !== null && value !== undefined) params.set(key, String(value)); }); return this.request("/api/v1/libraries/" + encodeURIComponent(id) + "/items?" + params.toString()); },
  item(id) { return this.request("/api/v1/items/" + encodeURIComponent(id)); },
  children(id, filters = {}) { const params = new URLSearchParams({ page: "1", pageSize: "60" }); Object.entries(filters).forEach(([key, value]) => { if (value !== "" && value !== null && value !== undefined) params.set(key, String(value)); }); return this.request("/api/v1/items/" + encodeURIComponent(id) + "/children?" + params.toString()); },
  playback(id) { return this.request("/api/v1/items/" + encodeURIComponent(id) + "/playback"); },
  favorite(id, favorite) { return this.request("/api/v1/items/" + encodeURIComponent(id) + "/favorite", { method: "PUT", headers: { "x-csrf-token": readCookie("lux_csrf") }, body: JSON.stringify({ favorite }) }); },
  played(id, played) { return this.request("/api/v1/items/" + encodeURIComponent(id) + "/played", { method: "PUT", headers: { "x-csrf-token": readCookie("lux_csrf") }, body: JSON.stringify({ played }) }); },
  search(query) { return this.request("/api/v1/search?q=" + encodeURIComponent(query) + "&page=1&pageSize=60"); },
  adminUsers() { return this.request("/api/v1/admin/users"); },
  createUser(data) { return this.request("/api/v1/admin/users", { method: "POST", headers: { "x-csrf-token": readCookie("lux_csrf") }, body: JSON.stringify(data) }); },
  disableUser(id) { return this.request("/api/v1/admin/users/" + encodeURIComponent(id), { method: "DELETE", headers: { "x-csrf-token": readCookie("lux_csrf") } }); },
  updateUser(id, data) { return this.request("/api/v1/admin/users/" + encodeURIComponent(id), { method: "PATCH", headers: { "x-csrf-token": readCookie("lux_csrf") }, body: JSON.stringify(data) }); },
  userLibraryAccess(id) { return this.request("/api/v1/admin/users/" + encodeURIComponent(id) + "/libraries"); },
  setLibraryAccess(userId, libraryId, canView) { return this.request("/api/v1/admin/users/" + encodeURIComponent(userId) + "/libraries/" + encodeURIComponent(libraryId), { method: "PATCH", headers: { "x-csrf-token": readCookie("lux_csrf") }, body: JSON.stringify({ canView }) }); },
  adminLibraries() { return this.request("/api/v1/admin/libraries"); },
  createLibrary(data) { return this.request("/api/v1/admin/libraries", { method: "POST", headers: { "x-csrf-token": readCookie("lux_csrf") }, body: JSON.stringify(data) }); },
  updateLibrary(id, data) { return this.request("/api/v1/admin/libraries/" + encodeURIComponent(id), { method: "PATCH", headers: { "x-csrf-token": readCookie("lux_csrf") }, body: JSON.stringify(data) }); },
  deleteLibrary(id) { return this.request("/api/v1/admin/libraries/" + encodeURIComponent(id), { method: "DELETE", headers: { "x-csrf-token": readCookie("lux_csrf") } }); },
  addLibraryRoot(id, path) { return this.request("/api/v1/admin/libraries/" + encodeURIComponent(id) + "/roots", { method: "POST", headers: { "x-csrf-token": readCookie("lux_csrf") }, body: JSON.stringify({ path }) }); },
  deleteLibraryRoot(libraryId, rootId) { return this.request("/api/v1/admin/libraries/" + encodeURIComponent(libraryId) + "/roots/" + encodeURIComponent(rootId), { method: "DELETE", headers: { "x-csrf-token": readCookie("lux_csrf") } }); },
  scanLibrary(id) { return this.request("/api/v1/admin/libraries/" + encodeURIComponent(id) + "/scan", { method: "POST", headers: { "x-csrf-token": readCookie("lux_csrf") } }); },
  adminJobs(status = "") { return this.request("/api/v1/admin/jobs?page=1&pageSize=50" + (status ? "&status=" + encodeURIComponent(status) : "")); },
  adminJob(id) { return this.request("/api/v1/admin/jobs/" + encodeURIComponent(id)); },
  adminJobEvents(id, filters = {}) { const params = new URLSearchParams({ page: String(filters.page || 1), pageSize: "100" }); if (filters.level) params.set("level", filters.level); if (filters.eventCode) params.set("eventCode", filters.eventCode); return this.request("/api/v1/admin/jobs/" + encodeURIComponent(id) + "/events?" + params.toString()); },
  cancelJob(id) { return this.request("/api/v1/admin/jobs/" + encodeURIComponent(id) + "/cancel", { method: "POST", headers: { "x-csrf-token": readCookie("lux_csrf") } }); },
  retryJob(id) { return this.request("/api/v1/admin/jobs/" + encodeURIComponent(id) + "/retry", { method: "POST", headers: { "x-csrf-token": readCookie("lux_csrf") } }); },
  audit() { return this.request("/api/v1/admin/audit?page=1&pageSize=50"); },
  pendingMetadata() { return this.request("/api/v1/admin/metadata/pending?page=1&pageSize=50"); },
  adminCandidates(itemId) { return this.request("/api/v1/admin/items/" + encodeURIComponent(itemId) + "/identify/candidates?page=1&pageSize=50"); },
  searchCandidates(itemId, query, year) { return this.request("/api/v1/admin/items/" + encodeURIComponent(itemId) + "/identify/candidates", { method: "POST", headers: { "x-csrf-token": readCookie("lux_csrf") }, body: JSON.stringify({ query, year: year || undefined }) }); },
  selectCandidate(itemId, candidateId, mode) { return this.request("/api/v1/admin/items/" + encodeURIComponent(itemId) + "/identify/candidates/" + encodeURIComponent(candidateId) + "/select", { method: "POST", headers: { "x-csrf-token": readCookie("lux_csrf") }, body: JSON.stringify({ mode }) }); },
  adminImages(itemId) { return this.request("/api/v1/admin/items/" + encodeURIComponent(itemId) + "/images"); },
  deleteAdminImage(itemId, imageId) { return this.request("/api/v1/admin/items/" + encodeURIComponent(itemId) + "/images/" + encodeURIComponent(imageId), { method: "DELETE", headers: { "x-csrf-token": readCookie("lux_csrf") } }); },
  ready() { return fetch("/health/ready", { credentials: "same-origin" }).then((response) => response.json()); },
  progress(id, positionTicks, durationTicks) { return this.request("/api/v1/items/" + encodeURIComponent(id) + "/progress", { method: "POST", headers: { "x-csrf-token": readCookie("lux_csrf") }, body: JSON.stringify({ positionTicks, durationTicks }) }); },
};

function readCookie(name) {
  const found = document.cookie.split("; ").find((part) => part.startsWith(name + "="));
  return found ? found.slice(name.length + 1) : "";
}

function field(form, name) {
  return form.elements.namedItem(name);
}

function escapeHtml(value) {
  return String(value ?? "").replace(/[&<>"']/g, (character) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[character]);
}

function imageUrl(item) {
  return item?.imageTags?.poster ? "/api/v1/items/" + encodeURIComponent(item.id) + "/images/poster" : "";
}

function poster(item, className = "poster") {
  const url = imageUrl(item);
  return url
    ? "<img class=\"" + className + "\" src=\"" + url + "\" alt=\"" + escapeHtml(item.title || item.name) + "\" loading=\"lazy\">"
    : "<div class=\"" + className + " poster-placeholder\" role=\"img\" aria-label=\"暂无海报\">暂无海报</div>";
}

function loading() { return "<div class=\"loading\" aria-busy=\"true\">正在整理你的媒体…</div>"; }
function titleForRoute() {
  if (state.route === "libraries") return "媒体库";
  if (state.route === "library") return state.libraries.find((library) => library.id === state.libraryId)?.name || "媒体库内容";
  if (state.route === "search") return "搜索";
  if (state.route === "item") return state.item?.title || "详情";
  if (state.route === "admin") return "管理控制台";
  if (state.route === "account") return "账户与会话";
  return "你的片单";
}

function render() {
  if (state.initialized === false) return renderSetup();
  if (!state.user) return renderAuth();
  const error = state.error ? "<div class=\"notice error\" role=\"alert\">" + escapeHtml(state.error) + "</div>" : "";
  const notice = state.notice ? "<div class=\"notice\" role=\"status\">" + escapeHtml(state.notice) + "</div>" : "";
  app.innerHTML = "<div class=\"shell\"><aside class=\"sidebar\">" + brand() + nav() + account() + "</aside><div><nav class=\"mobile-nav\"><strong>Lux</strong><button class=\"button secondary\" data-action=\"logout\">退出</button></nav><main class=\"content\"><header class=\"topbar\"><div><span class=\"eyebrow\">Personal media</span><h1>" + titleForRoute() + "</h1></div><form class=\"search-form\" data-action=\"search\"><input class=\"search-box\" name=\"q\" type=\"search\" placeholder=\"搜索电影、剧集或别名\" aria-label=\"搜索\"></form></header>" + error + notice + "<section id=\"view\">" + loading() + "</section></main></div></div>";
  bind();
  loadRoute();
}

function brand() { return "<div class=\"brand\"><strong>Lux</strong><span>quietly yours</span></div>"; }
function nav() {
  const homeCurrent = state.route === "home" ? "page" : "false";
  const libraryCurrent = state.route === "libraries" ? "page" : "false";
  const admin = state.user.canManageServer ? "<button data-route=\"admin\" aria-current=\"" + (state.route === "admin" ? "page" : "false") + "\">管理</button>" : "";
  return "<nav class=\"nav\" aria-label=\"主导航\"><button data-route=\"home\" aria-current=\"" + homeCurrent + "\">首页</button><button data-route=\"libraries\" aria-current=\"" + libraryCurrent + "\">媒体库</button><button data-route=\"account\" aria-current=\"" + (state.route === "account" ? "page" : "false") + "\">账户</button>" + admin + "</nav>";
}
function account() {
  return "<div class=\"sidebar-footer\"><small>" + escapeHtml(state.user.displayName || state.user.usernameNormalized) + "</small><button class=\"button secondary\" data-route=\"account\" style=\"margin-top:.7rem;width:100%\">账户与会话</button><button class=\"button secondary\" data-action=\"logout\" style=\"margin-top:.7rem;width:100%\">退出登录</button></div>";
}

function renderAuth() {
  const error = state.error ? "<div class=\"notice error\" role=\"alert\">" + escapeHtml(state.error) + "</div>" : "";
  const notice = state.setupNotice ? "<div class=\"notice\" role=\"status\">" + escapeHtml(state.setupNotice) + "</div>" : "";
  app.innerHTML = "<div class=\"auth-layout\"><section class=\"auth-card\"><span class=\"eyebrow\">Personal media</span><h1 style=\"margin-top:.7rem\">Lux</h1><p>把你的媒体库安静地放在自己的设备上。</p>" + notice + error + "<form data-action=\"login\"><div class=\"field\"><label for=\"username\">用户名</label><input id=\"username\" name=\"username\" autocomplete=\"username\" required></div><div class=\"field\"><label for=\"password\">密码</label><input id=\"password\" name=\"password\" type=\"password\" autocomplete=\"current-password\" required></div><div class=\"form-actions\"><button class=\"button\" type=\"submit\">登录</button></div></form></section></div>";
  bind();
}

function renderAccount(sessions) {
  const rows = sessions.map((session) => `<tr><td>${session.isCurrent ? "当前浏览器" : "其他会话"}</td><td>${escapeHtml(new Date((session.updatedAt || session.createdAt) * 1000).toLocaleString())}</td><td>${session.isCurrent ? "当前使用中" : `<button class="button secondary" type="button" data-revoke-session="${escapeHtml(session.id)}">撤销</button>`}</td></tr>`).join("");
  return `<section class="section"><div class="section-heading"><h2>账户与会话</h2><span>${escapeHtml(state.user.displayName || state.user.usernameNormalized)}</span></div><p>当前密码和权限由管理员管理。你可以撤销其他浏览器上的 Lux Web 会话。</p><div class="table-wrap"><table><thead><tr><th>设备</th><th>最近活动</th><th>状态</th></tr></thead><tbody>${rows || "<tr><td colspan=\"3\">暂无会话</td></tr>"}</tbody></table></div></section>`;
}

function renderSetup() {
  const error = state.error ? "<div class=\"notice error\" role=\"alert\">" + escapeHtml(state.error) + "</div>" : "";
  app.innerHTML = "<div class=\"auth-layout\"><section class=\"auth-card\"><span class=\"eyebrow\">First run</span><h1 style=\"margin-top:.7rem\">开始使用 Lux</h1><p>先创建服务器管理员。TMDb 和首个媒体库都可以跳过，之后在管理控制台配置。</p>" + error + "<form data-action=\"setup\"><div class=\"field\"><label for=\"setup-username\">管理员用户名</label><input id=\"setup-username\" name=\"username\" autocomplete=\"username\" required></div><div class=\"field\"><label for=\"setup-display-name\">显示名称</label><input id=\"setup-display-name\" name=\"displayName\" autocomplete=\"name\"></div><div class=\"field\"><label for=\"setup-password\">管理员密码</label><input id=\"setup-password\" name=\"password\" type=\"password\" autocomplete=\"new-password\" minlength=\"8\" required></div><fieldset class=\"setup-options\"><legend>可选设置</legend><div class=\"field\"><label for=\"setup-tmdb-token\">TMDb Read Access Token</label><input id=\"setup-tmdb-token\" name=\"tmdbToken\" type=\"password\" autocomplete=\"off\" placeholder=\"可跳过\"><small>仅保存在服务端配置目录，不会返回给普通用户。</small></div><div class=\"field\"><label for=\"setup-library-name\">首个媒体库名称</label><input id=\"setup-library-name\" name=\"libraryName\" placeholder=\"可跳过\"></div><div class=\"field\"><label for=\"setup-library-kind\">媒体库类型</label><select id=\"setup-library-kind\" name=\"libraryKind\"><option value=\"MIXED\">混合</option><option value=\"MOVIE\">电影</option><option value=\"SERIES\">剧集</option></select></div><div class=\"field\"><label for=\"setup-library-root\">媒体库根路径</label><input id=\"setup-library-root\" name=\"libraryRoot\" placeholder=\"可跳过，例如 /media\"><small>填写后服务端会检查目录是否存在且可读。</small></div></fieldset><div class=\"form-actions\"><button class=\"button\" type=\"submit\">完成初始化</button></div></form></section></div>";
  bind();
}

async function loadRoute() {
  const view = document.querySelector("#view");
  if (!view) return;
  try {
    if (state.route === "home") {
      state.home = await api.home();
      state.libraries = state.home?.libraries || (await api.libraries()).libraries || [];
      view.innerHTML = renderHome();
    } else if (state.route === "libraries") {
      state.libraries = state.libraries.length ? state.libraries : (await api.libraries()).libraries || [];
      view.innerHTML = renderLibraries();
    } else if (state.route === "library") {
      state.libraries = state.libraries.length ? state.libraries : (await api.libraries()).libraries || [];
      const result = await api.libraryItems(state.libraryId, state.libraryFilters);
      const library = state.libraries.find((entry) => entry.id === state.libraryId);
      view.innerHTML = renderLibraryItems(result, library);
    } else if (state.route === "search") {
      const result = await api.search(state.query);
      view.innerHTML = renderGrid(result.items || [], "搜索“" + escapeHtml(state.query) + "”");
    } else if (state.route === "item") {
      state.item = await api.item(state.itemId);
      if (state.user?.canManageServer) {
        const [images, candidates] = await Promise.all([api.adminImages(state.itemId), api.adminCandidates(state.itemId)]);
        state.itemImages = images.images || [];
        state.itemCandidates = candidates.items || [];
      } else {
        state.itemImages = [];
        state.itemCandidates = [];
      }
      state.playback = await api.playback(state.itemId);
      state.children = ["SERIES", "BOX_SET"].includes(state.item.itemType) ? await api.children(state.itemId) : null;
      view.innerHTML = renderDetail(state.item, state.playback, state.children, state.itemImages, state.itemCandidates);
    } else if (state.route === "admin") {
      const [ready, libraries, users, audit, pending, jobs] = await Promise.all([api.ready(), api.adminLibraries(), api.adminUsers(), api.audit(), api.pendingMetadata(), api.adminJobs()]);
      const accessEntries = await Promise.all((users.users || []).map(async (user) => [user.id, (await api.userLibraryAccess(user.id)).libraryIds || []]));
      state.admin = { ready, libraries: libraries.libraries || [], users: users.users || [], audit: audit.events || [], pending: pending.items || [], jobs: jobs.jobs || [], access: Object.fromEntries(accessEntries), jobDetail: state.admin?.jobDetail || null, jobEvents: state.admin?.jobEvents || [], jobEventsTotal: state.admin?.jobEventsTotal || 0, jobEventsPage: state.admin?.jobEventsPage || 1, jobEventFilters: state.admin?.jobEventFilters || {} };
      view.innerHTML = renderAdmin();
    } else if (state.route === "account") {
      const sessions = await api.sessions();
      view.innerHTML = renderAccount(sessions.sessions || []);
    }
    bind();
  } catch (error) {
    state.error = error.message;
    view.innerHTML = "<div class=\"empty\"><h2>暂时无法读取</h2><p>" + escapeHtml(error.message) + "</p><button class=\"button secondary\" data-action=\"retry\">重试</button></div>";
    bind();
  }
}

function renderHome() {
  const continueWatching = state.home?.continueWatching || [];
  const libraries = state.home?.libraries || state.libraries;
  const progress = continueWatching.length ? "<section class=\"section\"><div class=\"section-heading\"><h2>继续观看</h2><span>" + continueWatching.length + " 个进行中</span></div>" + renderGrid(continueWatching) + "</section>" : "";
  const cards = libraries.length ? libraries.map(libraryCard).join("") : "<div class=\"empty\"><h3>还没有可见媒体库</h3><p>请联系管理员授予媒体库访问权限。</p></div>";
  return progress + "<section class=\"section\"><div class=\"section-heading\"><h2>媒体库</h2><span>只显示你有权限访问的库</span></div><div class=\"library-grid\">" + cards + "</div></section>";
}

function libraryCard(library) {
  return "<button class=\"library-card\" data-library=\"" + escapeHtml(library.id) + "\"><span class=\"eyebrow\">" + escapeHtml(library.kind || "library") + "</span><strong>" + escapeHtml(library.name) + "</strong><span class=\"media-meta\">打开媒体库 →</span></button>";
}
function renderLibraries() {
  const content = state.libraries.length ? state.libraries.map(libraryCard).join("") : "<div class=\"empty\"><h2>没有媒体库</h2><p>当前账户没有可见媒体库。</p></div>";
  return "<section class=\"section\"><div class=\"library-grid\">" + content + "</div></section>";
}
function renderLibraryItems(result, library) {
  const filters = state.libraryFilters || {};
  const selected = (name, value) => filters[name] === value ? " selected" : "";
  const form = `<form class="filter-form" data-action="library-filter"><label>类型 <select name="item_type"><option value="">全部</option><option value="movie"${selected("item_type", "movie")}>电影</option><option value="series"${selected("item_type", "series")}>剧集</option><option value="episode"${selected("item_type", "episode")}>单集</option></select></label><label>年份 <input name="year" type="number" min="1800" max="2200" value="${escapeHtml(filters.year || "")}" placeholder="2024"></label><label>观看 <select name="is_played"><option value="">全部</option><option value="true"${selected("is_played", "true")}>已看</option><option value="false"${selected("is_played", "false")}>未看</option></select></label><label>收藏 <select name="is_favorite"><option value="">全部</option><option value="true"${selected("is_favorite", "true")}>已收藏</option><option value="false"${selected("is_favorite", "false")}>未收藏</option></select></label><label>排序 <select name="sort_by"><option value="">名称</option><option value="DateCreated"${selected("sort_by", "DateCreated")}>最近添加</option></select></label><label>顺序 <select name="sort_order"><option value="Ascending"${selected("sort_order", "Ascending")}>正序</option><option value="Descending"${selected("sort_order", "Descending")}>倒序</option></select></label><button class="button" type="submit">应用筛选</button><button class="button secondary" type="button" data-action="clear-library-filter">清除</button></form>`;
  return "<section class=\"section\"><div class=\"section-heading\"><h2>" + escapeHtml(library?.name || "媒体库") + "</h2><span>" + (result.total || 0) + " 项</span></div>" + form + renderGrid(result.items || []) + "</section>";
}
function renderGrid(items, heading = "") {
  const title = heading ? "<div class=\"section-heading\" style=\"grid-column:1/-1\"><h2>" + heading + "</h2><span>" + items.length + " 项</span></div>" : "";
  const content = items.length ? items.map(mediaCard).join("") : "<div class=\"empty\" style=\"grid-column:1/-1\"><h3>没有找到内容</h3><p>试试其他关键词或筛选条件。</p></div>";
  return "<div class=\"media-grid\">" + title + content + "</div>";
}
function mediaCard(item) {
  const userData = item.userData || {};
  const status = [userData.isPlayed ? "已看" : "", userData.isFavorite ? "已收藏" : ""].filter(Boolean).join(" · ");
  return "<button class=\"media-card\" data-item=\"" + escapeHtml(item.id) + "\">" + poster(item) + "<span class=\"media-card-body\"><strong>" + escapeHtml(item.title || item.name) + "</strong><span class=\"media-meta\">" + escapeHtml([item.productionYear || item.itemType || "", status].filter(Boolean).join(" · ")) + "</span></span></button>";
}
function renderDetail(item, playback = {}, children = null, images = [], candidates = []) {
  const sources = item.mediaSources || [];
  const chips = sources.map((source) => "<span class=\"chip\">" + escapeHtml(source.qualityLabel || source.editionName || source.container || "source") + "</span>").join("");
  const buttons = sources.map((source, index) => "<button class=\"button secondary\" data-source=\"" + escapeHtml(source.id) + "\" aria-pressed=\"" + (index === 0) + "\">" + escapeHtml(source.qualityLabel || source.editionName || source.container || "版本 " + (index + 1)) + "</button>").join("");
  const player = sources.length ? "<div class=\"source-list\" aria-label=\"媒体版本\">" + buttons + "</div><p class=\"player-status\" data-player-status role=\"status\"></p><video class=\"player\" controls preload=\"metadata\" data-player aria-label=\"播放 " + escapeHtml(item.title || item.name) + "\" src=\"/api/v1/items/" + encodeURIComponent(item.id) + "/stream?sourceId=" + encodeURIComponent(sources[0].id) + "\"></video>" : "";
  const favoriteLabel = playback.isFavorite ? "取消收藏" : "收藏";
  const playedLabel = playback.isPlayed ? "标记未看" : "标记已看";
  const userData = "<div class=\"chips\"><span class=\"chip\">" + (playback.isPlayed ? "已看" : "未看") + "</span>" + (playback.isFavorite ? "<span class=\"chip\">已收藏</span>" : "") + (playback.positionTicks ? "<span class=\"chip\">已播放 " + Math.round(playback.positionTicks / 10000000) + " 秒</span>" : "") + "</div>";
  const childrenPanel = children ? `<section class="children-panel" id="children-panel">${renderChildrenPanel(item, children)}</section>` : "";
  return `<a class="back-link" href="#home" data-route="home">← 返回</a><article class="detail"><div>${poster(item, "detail-poster")}</div><div class="detail-copy"><span class="eyebrow">${escapeHtml(item.itemType || item.type || "media")}</span><h2 style="margin-top:.6rem">${escapeHtml(item.title || item.name)}</h2><div class="chips">${item.productionYear ? `<span class="chip">${item.productionYear}</span>` : ""}${chips}</div>${userData}<p>${escapeHtml(item.overview || "暂无简介。")}</p><div class="form-actions"><button class="button secondary" data-action="toggle-favorite" aria-pressed="${Boolean(playback.isFavorite)}">${favoriteLabel}</button><button class="button secondary" data-action="toggle-played" aria-pressed="${Boolean(playback.isPlayed)}">${playedLabel}</button></div>${childrenPanel}${renderAdminImages(item, images)}${renderAdminCandidates(item, candidates)}${player}</div></article>`;
}

function renderAdminImages(item, images) {
  if (!state.user?.canManageServer) return "";
  const rows = images.map((image) => {
    const type = image.imageType === "FANART" ? "fanart" : "poster";
    return `<li><img src="/api/v1/items/${encodeURIComponent(item.id)}/images/${type}" alt="${escapeHtml(type)}"><div><strong>${escapeHtml(image.imageType)} #${image.imageIndex}</strong><span>${escapeHtml(image.source || "LOCAL")} · ${escapeHtml(String(image.fileSize || 0))} bytes</span></div><button class="button secondary" type="button" data-delete-image="${escapeHtml(image.id)}">删除图片</button></li>`;
  }).join("");
  return `<section class="admin-images"><div class="section-heading"><h3>图片管理</h3><span>删除会移除索引和媒体目录中的文件</span></div><ul class="admin-list">${rows || "<li><span>暂无图片索引</span></li>"}</ul></section>`;
}

function renderAdminCandidates(item, candidates) {
  if (!state.user?.canManageServer) return "";
  const rows = candidates.map((candidate) => {
    const data = candidate.candidate && typeof candidate.candidate === "object" ? candidate.candidate : {};
    const title = data.title || data.originalTitle || candidate.providerId || "未命名候选";
    const diffs = (candidate.fieldDiffs || []).map((diff) => `<li><strong>${escapeHtml(diff.field)}</strong><span>${escapeHtml(String(diff.current ?? "暂无"))} → ${escapeHtml(String(diff.candidate ?? "暂无"))}</span></li>`).join("");
    return `<article class="candidate-card"><div class="section-heading"><div><h4>${escapeHtml(title)}</h4><span>${escapeHtml(candidate.provider || "provider")} · ${escapeHtml(candidate.providerId || "")}</span></div><span class="chip">${escapeHtml(String(candidate.score ?? 0))}</span></div><ul class="diff-list">${diffs || "<li><span>没有可展示的字段差异</span></li>"}</ul><div class="form-actions"><button class="button secondary" data-select-candidate="${escapeHtml(candidate.itemId)}|${escapeHtml(candidate.id)}|fillMissing">仅补缺</button><button class="button" data-select-candidate="${escapeHtml(candidate.itemId)}|${escapeHtml(candidate.id)}|refreshUnlocked">刷新未锁定</button></div></article>`;
  }).join("");
  return `<section class="admin-images"><div class="section-heading"><h3>重新识别</h3><span>搜索 TMDb 后选择候选写回本地 NFO</span></div><form class="admin-form compact-form" data-action="search-candidates"><input name="query" value="${escapeHtml(item.title || "")}" placeholder="TMDb 搜索关键词" aria-label="TMDb 搜索关键词" required><input name="year" type="number" min="1800" max="2200" placeholder="年份（可选）" aria-label="年份（可选）"><button class="button secondary" type="submit">搜索候选</button></form><div class="candidate-list">${rows || "<div class=\"empty\"><p>还没有候选。可以按标题重新搜索。</p></div>"}</div></section>`;
}

function renderChildrenPanel(item, result, showingEpisodes = false) {
  const items = result.items || [];
  if (item.itemType === "BOX_SET") return `<div class="section-heading"><h3>合集成员</h3><span>${result.total || items.length} 项</span></div>${renderGrid(items)}`;
  if (showingEpisodes) return `<div class="section-heading"><h3>单集</h3><button class="button secondary" type="button" data-show-seasons>返回季度</button></div>${renderGrid(items)}`;
  const seasons = items.map((season) => `<button class="library-card" data-season="${escapeHtml(season.id)}"><span class="eyebrow">Season</span><strong>${escapeHtml(season.title || season.name)}</strong><span class="media-meta">打开单集 →</span></button>`).join("");
  return `<div class="section-heading"><h3>季度</h3><span>${result.total || items.length} 个季度</span></div><div class="library-grid">${seasons || "<div class=\"empty\"><span>暂无季度</span></div>"}</div>`;
}

function renderAdmin() {
  const { ready = {}, libraries = [], users = [], audit: events = [], pending = [], jobs = [], access = {} } = state.admin || {};
  const jobDetail = state.admin?.jobDetail || null;
  const userRows = users.map((user) => "<tr><td>" + escapeHtml(user.displayName || user.usernameNormalized) + "<small>" + escapeHtml(user.usernameNormalized) + "</small></td><td>" + (user.isDisabled ? "已禁用" : user.canManageServer ? "管理员" : "普通用户") + "</td><td><a class=\"button secondary\" href=\"#user-" + escapeHtml(user.id) + "\">编辑</a> " + (user.isDisabled ? "" : "<button class=\"button secondary\" data-disable-user=\"" + escapeHtml(user.id) + "\">禁用</button>") + "</td></tr>").join("");
  const userEditors = users.map((user) => renderUserEditor(user, libraries, new Set(access[user.id] || []))).join("");
  const libraryCards = libraries.map(renderAdminLibrary).join("");
  const auditRows = events.slice(0, 8).map((event) => "<li><strong>" + escapeHtml(event.eventType) + "</strong><span>" + escapeHtml(event.actorUsername || "system") + " · " + escapeHtml(event.targetId || "") + "</span></li>").join("");
  return "<section class=\"section\"><div class=\"admin-cards\"><div class=\"admin-card\"><span class=\"eyebrow\">Health</span><strong>" + escapeHtml(ready.status || "unknown") + "</strong><span>schema " + escapeHtml(ready.schemaVersion || "—") + "</span></div><div class=\"admin-card\"><span class=\"eyebrow\">Libraries</span><strong>" + libraries.length + "</strong><span>已配置媒体库</span></div><div class=\"admin-card\"><span class=\"eyebrow\">Users</span><strong>" + users.length + "</strong><span>账户</span></div></div></section>" +
    "<section class=\"section\"><div class=\"section-heading\"><h2>创建媒体库</h2><span>创建后再添加一个或多个根路径</span></div><form class=\"admin-form\" data-action=\"create-library\"><input name=\"name\" placeholder=\"媒体库名称\" aria-label=\"媒体库名称\" required><select name=\"kind\" aria-label=\"媒体库类型\"><option value=\"MOVIE\">电影</option><option value=\"SERIES\">剧集</option><option value=\"MIXED\">混合</option></select><label class=\"check\"><input name=\"realtimeWatchEnabled\" type=\"checkbox\"> 实时监听</label><button class=\"button\" type=\"submit\">创建媒体库</button></form></section>" +
    "<section class=\"section\"><div class=\"section-heading\"><h2>媒体库与扫描</h2><span>计划字段留空可清除</span></div><div class=\"admin-library-grid\">" + (libraryCards || "<div class=\"empty\"><h3>还没有媒体库</h3><p>先创建一个媒体库，再添加根路径。</p></div>") + "</div></section>" +
    "<section class=\"section\"><div class=\"section-heading\"><h2>任务</h2><span>失败任务可重试，运行中任务可取消</span></div><form class=\"filter-form\" data-action=\"job-filter\"><label>状态 <select name=\"status\"><option value=\"\">全部</option><option value=\"RUNNING\">运行中</option><option value=\"FAILED\">失败</option><option value=\"CANCELLED\">已取消</option><option value=\"COMPLETED\">已完成</option></select></label><button class=\"button secondary\" type=\"submit\">筛选任务</button></form>" + renderJobs(jobs, libraries) + (jobDetail ? renderJobDetail(jobDetail, state.admin?.jobEvents || [], state.admin?.jobEventsTotal || 0, state.admin?.jobEventsPage || 1) : "") + "</section>" +
    "<section class=\"section\"><div class=\"section-heading\"><h2>用户与权限</h2><span>密码为空表示不修改</span></div><form class=\"admin-form\" data-action=\"create-user\"><input name=\"username\" placeholder=\"用户名\" aria-label=\"用户名\" required><input name=\"displayName\" placeholder=\"显示名称\" aria-label=\"显示名称\"><input name=\"password\" type=\"password\" placeholder=\"初始密码\" aria-label=\"初始密码\" required><label class=\"check\"><input name=\"isAdmin\" type=\"checkbox\"> 管理员</label><button class=\"button\" type=\"submit\">创建用户</button></form><div class=\"table-wrap\"><table><thead><tr><th>用户</th><th>角色</th><th>操作</th></tr></thead><tbody>" + userRows + "</tbody></table></div>" + userEditors + "</section>" +
    "<section class=\"section\"><div class=\"section-heading\"><h2>待处理元数据</h2><span>候选写回前请检查差异</span></div><div class=\"candidate-list\">" + renderPendingCandidates(pending) + "</div></section>" +
    "<section class=\"section\"><div class=\"section-heading\"><h2>最近审计</h2><span>只展示最近 8 条</span></div><ul class=\"admin-list\">" + (auditRows || "<li><span>暂无管理操作</span></li>") + "</ul></section>";
}

function renderJobs(jobs, libraries) {
  if (!jobs.length) return "<div class=\"empty\"><h3>暂无任务</h3></div>";
  const libraryName = (id) => libraries.find((library) => library.id === id)?.name || id;
  const rows = jobs.map((job) => {
    const action = `<button class="button secondary" data-job-detail="${escapeHtml(job.id)}">详情</button> ` + (["FAILED", "CANCELLED"].includes(job.status) ? `<button class="button secondary" data-retry-job="${escapeHtml(job.id)}">重试</button>` : ["PENDING", "RUNNING"].includes(job.status) ? `<button class="button secondary" data-cancel-job="${escapeHtml(job.id)}">取消</button>` : "");
    const progress = job.totalCount ? " · " + job.processedCount + "/" + job.totalCount : "";
    return "<tr><td>" + escapeHtml(libraryName(job.libraryId)) + "<small>" + escapeHtml(job.jobType) + "</small></td><td>" + escapeHtml(job.status) + escapeHtml(progress) + "</td><td>" + escapeHtml(job.error || "") + "</td><td>" + action + "</td></tr>";
  }).join("");
  return "<div class=\"table-wrap\"><table><thead><tr><th>媒体库/类型</th><th>状态</th><th>错误</th><th>操作</th></tr></thead><tbody>" + rows + "</tbody></table></div>";
}

function renderJobDetail(job, events = [], total = 0, page = 1) {
  const progress = `${job.processedCount || 0}/${job.totalCount || 0}`;
  const filters = state.admin?.jobEventFilters || {};
  const eventRows = events.map((event) => `<tr><td>${escapeHtml(event.level)}</td><td>${escapeHtml(event.eventCode)}</td><td>${escapeHtml(event.message)}</td><td>${escapeHtml(event.createdAt || "")}</td></tr>`).join("");
  const previous = page > 1 ? `<button class="button secondary" type="button" data-job-events-page="${page - 1}">上一页</button>` : "";
  const next = page * 100 < total ? `<button class="button secondary" type="button" data-job-events-page="${page + 1}">下一页</button>` : "";
  return `<article class="job-detail"><div class="section-heading"><h3>任务详情</h3><button class="button secondary" type="button" data-close-job-detail>关闭</button></div><dl><div><dt>ID</dt><dd>${escapeHtml(job.id)}</dd></div><div><dt>状态</dt><dd>${escapeHtml(job.status)}</dd></div><div><dt>进度</dt><dd>${escapeHtml(progress)}</dd></div><div><dt>游标</dt><dd>${escapeHtml(job.cursor || "—")}</dd></div><div><dt>代次</dt><dd>${escapeHtml(job.generation || "—")}</dd></div><div><dt>错误</dt><dd>${escapeHtml(job.error || "—")}</dd></div></dl><form class="filter-form" data-action="job-event-filter"><label>级别 <select name="level"><option value="">全部</option><option value="ERROR"${filters.level === "ERROR" ? " selected" : ""}>错误</option><option value="WARN"${filters.level === "WARN" ? " selected" : ""}>警告</option><option value="INFO"${filters.level === "INFO" ? " selected" : ""}>信息</option></select></label><label>事件代码 <input name="eventCode" value="${escapeHtml(filters.eventCode || "")}" placeholder="JOB_FAILED"></label><button class="button secondary" type="submit">筛选日志</button></form><div class="table-wrap"><table><thead><tr><th>级别</th><th>事件</th><th>消息</th><th>时间</th></tr></thead><tbody>${eventRows || "<tr><td colspan=\"4\">暂无日志</td></tr>"}</tbody></table></div><div class="form-actions"><span class="media-meta">共 ${escapeHtml(total)} 条，第 ${page} 页</span>${previous}${next}</div></article>`;
}

function renderPendingCandidates(candidates) {
  if (!candidates.length) return "<div class=\"empty\"><h3>没有待处理候选</h3><p>扫描和识别产生的低置信候选会显示在这里。</p></div>";
  return candidates.map((candidate) => {
    const candidateData = candidate.candidate && typeof candidate.candidate === "object" ? candidate.candidate : {};
    const candidateTitle = candidateData.title || candidateData.name || candidate.providerId || "候选元数据";
    const diffs = (candidate.fieldDiffs || []).map((diff) => "<li><strong>" + escapeHtml(diff.field) + "</strong><span>" + escapeHtml(String(diff.current ?? "暂无")) + " → " + escapeHtml(String(diff.candidate ?? "暂无")) + "</span></li>").join("");
    return "<article class=\"candidate-card\"><div class=\"section-heading\"><div><h3>" + escapeHtml(candidate.itemTitle || "未命名条目") + "</h3><span>" + escapeHtml(candidate.provider || "provider") + " · " + escapeHtml(candidateTitle) + "</span></div><span class=\"chip\">" + escapeHtml(candidate.status || "PENDING") + "</span></div><ul class=\"diff-list\">" + (diffs || "<li><span>没有可展示的字段差异</span></li>") + "</ul><div class=\"form-actions\"><button class=\"button secondary\" data-select-candidate=\"" + escapeHtml(candidate.itemId) + "|" + escapeHtml(candidate.id) + "|fillMissing\">仅补缺</button><button class=\"button\" data-select-candidate=\"" + escapeHtml(candidate.itemId) + "|" + escapeHtml(candidate.id) + "|refreshUnlocked\">刷新未锁定</button></div></article>";
  }).join("");
}

function renderAdminLibrary(library) {
  const roots = (library.roots || []).map((root) => "<li><div><strong>" + escapeHtml(root.displayPath || root.canonicalPath) + "</strong><span>" + (root.isAvailable ? "可用" : "不可用") + (root.isWritable ? " · 可写" : " · 只读") + "</span></div><button class=\"button secondary\" type=\"button\" data-delete-root=\"" + escapeHtml(root.id) + "\" data-library-id=\"" + escapeHtml(library.id) + "\">删除配置</button></li>").join("");
  return `<article class="admin-library"><div class="section-heading"><div><h3>${escapeHtml(library.name)}</h3><span>${escapeHtml(library.kind)} · ${library.isEnabled ? "已启用" : "已停用"} · ${library.realtimeWatchEnabled ? "实时监听" : "手动/计划扫描"}</span></div><div class="form-actions"><button class="button secondary" data-scan-library="${escapeHtml(library.id)}"${library.isEnabled ? "" : " disabled"}>开始扫描</button><button class="button secondary" data-delete-library="${escapeHtml(library.id)}">删除媒体库</button></div></div><ul class="admin-list">${roots || "<li><span>尚未添加根路径</span></li>"}</ul><form class="admin-form compact-form" data-action="add-root" data-library-id="${escapeHtml(library.id)}"><input name="path" placeholder="/Volumes/Media/Movies" aria-label="根路径" required><button class="button secondary" type="submit">添加根路径</button></form><form class="schedule-form" data-action="update-library" data-library-id="${escapeHtml(library.id)}"><label class="check"><input name="isEnabled" type="checkbox"${library.isEnabled ? " checked" : ""}> 启用媒体库</label><label>增量 <input name="incrementalSchedule" value="${escapeHtml(library.incrementalSchedule || "")}" placeholder="interval:30s"></label><label>调和 <input name="reconciliationSchedule" value="${escapeHtml(library.reconciliationSchedule || "")}" placeholder="cron:0 3 * * *"></label><label>元数据 <input name="metadataSchedule" value="${escapeHtml(library.metadataSchedule || "")}" placeholder="interval:6h"></label><label>扫描并发 <input name="scanConcurrency" type="number" min="1" max="64" value="${escapeHtml(library.scanConcurrency || "")}"></label><label>探测并发 <input name="probeConcurrency" type="number" min="1" max="64" value="${escapeHtml(library.probeConcurrency || "")}"></label><label class="check"><input name="realtimeWatchEnabled" type="checkbox"${library.realtimeWatchEnabled ? " checked" : ""}> 实时监听</label><button class="button secondary" type="submit">保存计划</button></form></article>`;
}

function renderUserEditor(user, libraries, granted) {
  const libraryChecks = libraries.map((library) => `<label class="check"><input type="checkbox" name="libraryAccess" value="${escapeHtml(library.id)}"${granted.has(library.id) ? " checked" : ""}> ${escapeHtml(library.name)}</label>`).join("");
  return `<details class="admin-user" id="user-${escapeHtml(user.id)}"><summary>编辑 ${escapeHtml(user.displayName || user.usernameNormalized)}</summary><form class="user-edit-form" data-action="update-user" data-user-id="${escapeHtml(user.id)}"><div class="admin-form"><input name="displayName" value="${escapeHtml(user.displayName || "")}" placeholder="显示名称" aria-label="显示名称"><input name="password" type="password" placeholder="新密码（可选）" aria-label="新密码"></div><div class="permission-grid"><label class="check"><input name="isAdmin" type="checkbox"${user.isAdmin ? " checked" : ""}> 管理员</label><label class="check"><input name="canManageServer" type="checkbox"${user.canManageServer ? " checked" : ""}> 管理服务器</label><label class="check"><input name="canRemoteAccess" type="checkbox"${user.canRemoteAccess ? " checked" : ""}> 允许远程</label><label class="check"><input name="canDownload" type="checkbox"${user.canDownload ? " checked" : ""}> 允许下载</label><label class="check"><input name="isDisabled" type="checkbox"${user.isDisabled ? " checked" : ""}> 已禁用</label></div><div class="permission-grid"><strong>媒体库访问</strong>${libraryChecks || "<span class=\"media-meta\">暂无媒体库</span>"}</div><button class="button" type="submit">保存用户权限</button></form></details>`;
}

function bind() {
  document.querySelectorAll("[data-route]").forEach((element) => element.addEventListener("click", (event) => { event.preventDefault(); state.route = element.dataset.route; state.error = ""; render(); }));
  document.querySelectorAll("[data-library]").forEach((element) => element.addEventListener("click", async () => {
    state.libraryId = element.dataset.library; state.libraryFilters = {}; state.route = "library"; state.error = ""; state.notice = ""; render();
  }));
  document.querySelectorAll("[data-item]").forEach((element) => element.addEventListener("click", () => { state.itemId = element.dataset.item; state.route = "item"; state.error = ""; render(); }));
  document.querySelectorAll("[data-source]").forEach((element) => element.addEventListener("click", () => {
    const player = document.querySelector("[data-player]");
    if (!player) return;
    player.src = "/api/v1/items/" + encodeURIComponent(state.item.id) + "/stream?sourceId=" + encodeURIComponent(element.dataset.source);
    document.querySelectorAll("[data-source]").forEach((button) => button.setAttribute("aria-pressed", String(button === element)));
    player.play().catch(() => {});
  }));
  document.querySelectorAll("form[data-action='library-filter']").forEach((form) => form.addEventListener("submit", (event) => {
    event.preventDefault();
    state.libraryFilters = Object.fromEntries(["item_type", "year", "is_played", "is_favorite", "sort_by", "sort_order"].map((name) => [name, form[name].value]).filter(([, value]) => value));
    state.route = "library"; state.error = ""; state.notice = ""; render();
  }));
  document.querySelectorAll("[data-action='clear-library-filter']").forEach((element) => element.addEventListener("click", () => { state.libraryFilters = {}; state.route = "library"; state.error = ""; state.notice = ""; render(); }));
  document.querySelectorAll("[data-action='toggle-favorite']").forEach((element) => element.addEventListener("click", async () => {
    try { await api.favorite(state.item.id, !state.playback?.isFavorite); state.notice = state.playback?.isFavorite ? "已取消收藏。" : "已加入收藏。"; state.error = ""; render(); }
    catch (error) { state.error = error.message; state.notice = ""; render(); }
  }));
  document.querySelectorAll("[data-action='toggle-played']").forEach((element) => element.addEventListener("click", async () => {
    try { await api.played(state.item.id, !state.playback?.isPlayed); state.notice = state.playback?.isPlayed ? "已标记为未看。" : "已标记为已看。"; state.error = ""; render(); }
    catch (error) { state.error = error.message; state.notice = ""; render(); }
  }));
  document.querySelectorAll("[data-season]").forEach((element) => element.addEventListener("click", async () => {
    try { state.children = await api.children(state.item.id, { itemType: "EPISODE", seasonId: element.dataset.season }); document.querySelector("#children-panel").innerHTML = renderChildrenPanel(state.item, state.children, true); bind(); }
    catch (error) { state.error = error.message; render(); }
  }));
  document.querySelectorAll("[data-show-seasons]").forEach((element) => element.addEventListener("click", async () => {
    try { state.children = await api.children(state.item.id); document.querySelector("#children-panel").innerHTML = renderChildrenPanel(state.item, state.children); bind(); }
    catch (error) { state.error = error.message; render(); }
  }));
  document.querySelectorAll("[data-player]").forEach((player) => {
    let lastReport = 0;
    player.addEventListener("error", () => {
      const status = document.querySelector("[data-player-status]");
      if (status) status.textContent = "浏览器无法播放此媒体编码，请尝试其他版本或使用支持该编码的客户端。";
    });
    player.addEventListener("timeupdate", () => {
      if (!state.item || player.currentTime - lastReport < 10) return;
      lastReport = player.currentTime;
      api.progress(state.item.id, Math.round(player.currentTime * 10000000), Number.isFinite(player.duration) ? Math.round(player.duration * 10000000) : null).catch(() => {});
    });
  });
  document.querySelectorAll("[data-disable-user]").forEach((element) => element.addEventListener("click", async () => {
    if (!window.confirm("确认禁用这个用户？")) return;
    try { await api.disableUser(element.dataset.disableUser); state.error = ""; render(); } catch (error) { state.error = error.message; render(); }
  }));
  document.querySelectorAll("[data-scan-library]").forEach((element) => element.addEventListener("click", async () => {
    try { await api.scanLibrary(element.dataset.scanLibrary); state.notice = "扫描任务已创建。"; state.error = ""; render(); }
    catch (error) { state.error = error.message; state.notice = ""; render(); }
  }));
  document.querySelectorAll("[data-delete-root]").forEach((element) => element.addEventListener("click", async () => {
    if (!window.confirm("只删除这条根路径配置，不会删除媒体文件。继续？")) return;
    try { await api.deleteLibraryRoot(element.dataset.libraryId, element.dataset.deleteRoot); state.error = ""; state.notice = "根路径配置已删除。"; render(); }
    catch (error) { state.error = error.message; state.notice = ""; render(); }
  }));
  document.querySelectorAll("[data-delete-library]").forEach((element) => element.addEventListener("click", async () => {
    if (!window.confirm("删除媒体库配置和索引数据，但不会删除媒体文件。继续？")) return;
    try { await api.deleteLibrary(element.dataset.deleteLibrary); state.error = ""; state.notice = "媒体库已删除。"; render(); }
    catch (error) { state.error = error.message; state.notice = ""; render(); }
  }));
  document.querySelectorAll("[data-delete-image]").forEach((element) => element.addEventListener("click", async () => {
    if (!window.confirm("删除这张图片及其索引？")) return;
    try { await api.deleteAdminImage(state.item.id, element.dataset.deleteImage); state.error = ""; state.notice = "图片已删除。"; render(); }
    catch (error) { state.error = error.message; state.notice = ""; render(); }
  }));
  document.querySelectorAll("form[data-action='search-candidates']").forEach((form) => form.addEventListener("submit", async (event) => {
    event.preventDefault();
    const query = field(form, "query").value.trim();
    const year = field(form, "year").value ? Number(field(form, "year").value) : undefined;
    try { const result = await api.searchCandidates(state.item.id, query, year); state.itemCandidates = result.items || []; state.error = ""; state.notice = "TMDb 候选已更新。"; render(); }
    catch (error) { state.error = error.message; state.notice = ""; render(); }
  }));
  document.querySelectorAll("[data-select-candidate]").forEach((element) => element.addEventListener("click", async () => {
    const [itemId, candidateId, mode] = element.dataset.selectCandidate.split("|");
    element.disabled = true;
    try { await api.selectCandidate(itemId, candidateId, mode); state.notice = "元数据候选已写回。"; state.error = ""; render(); }
    catch (error) { element.disabled = false; state.error = error.message; state.notice = ""; render(); }
  }));
  document.querySelectorAll("[data-cancel-job]").forEach((element) => element.addEventListener("click", async () => {
    try { await api.cancelJob(element.dataset.cancelJob); state.notice = "取消任务请求已提交。"; state.error = ""; render(); }
    catch (error) { state.error = error.message; state.notice = ""; render(); }
  }));
  document.querySelectorAll("[data-retry-job]").forEach((element) => element.addEventListener("click", async () => {
    try { await api.retryJob(element.dataset.retryJob); state.notice = "重试任务已创建。"; state.error = ""; render(); }
    catch (error) { state.error = error.message; state.notice = ""; render(); }
  }));
  document.querySelectorAll("[data-job-detail]").forEach((element) => element.addEventListener("click", async () => {
    try { const [job, events] = await Promise.all([api.adminJob(element.dataset.jobDetail), api.adminJobEvents(element.dataset.jobDetail)]); state.admin.jobDetail = job.job; state.admin.jobEvents = events.events || []; state.admin.jobEventsTotal = events.total || 0; state.admin.jobEventsPage = events.page || 1; state.admin.jobEventFilters = {}; state.error = ""; render(); }
    catch (error) { state.error = error.message; state.notice = ""; render(); }
  }));
  document.querySelectorAll("form[data-action='job-event-filter']").forEach((form) => form.addEventListener("submit", async (event) => {
    event.preventDefault();
    try { const filters = { level: form.level.value, eventCode: form.eventCode.value.trim(), page: 1 }; const result = await api.adminJobEvents(state.admin.jobDetail.id, filters); state.admin.jobEvents = result.events || []; state.admin.jobEventsTotal = result.total || 0; state.admin.jobEventsPage = result.page || 1; state.admin.jobEventFilters = filters; state.error = ""; render(); }
    catch (error) { state.error = error.message; state.notice = ""; render(); }
  }));
  document.querySelectorAll("[data-job-events-page]").forEach((element) => element.addEventListener("click", async () => {
    try { const result = await api.adminJobEvents(state.admin.jobDetail.id, { ...state.admin.jobEventFilters, page: Number(element.dataset.jobEventsPage) }); state.admin.jobEvents = result.events || []; state.admin.jobEventsTotal = result.total || 0; state.admin.jobEventsPage = result.page || 1; state.error = ""; render(); }
    catch (error) { state.error = error.message; state.notice = ""; render(); }
  }));
  document.querySelectorAll("[data-close-job-detail]").forEach((element) => element.addEventListener("click", () => {
    state.admin.jobDetail = null; state.admin.jobEvents = []; state.admin.jobEventsTotal = 0; render();
  }));
  document.querySelectorAll("[data-revoke-session]").forEach((element) => element.addEventListener("click", async () => {
    if (!window.confirm("撤销这个浏览器会话？")) return;
    try { await api.revokeSession(element.dataset.revokeSession); state.error = ""; state.notice = "会话已撤销。"; render(); }
    catch (error) { state.error = error.message; state.notice = ""; render(); }
  }));
  document.querySelectorAll("form[data-action='job-filter']").forEach((form) => form.addEventListener("submit", async (event) => {
    event.preventDefault();
    try { state.admin.jobs = (await api.adminJobs(form.status.value)).jobs || []; state.notice = "任务列表已更新。"; state.error = ""; render(); }
    catch (error) { state.error = error.message; state.notice = ""; render(); }
  }));
  document.querySelectorAll("[data-action='logout']").forEach((element) => element.addEventListener("click", async () => { try { await api.logout(); } finally { state.user = null; render(); } }));
  document.querySelectorAll("[data-action='retry']").forEach((element) => element.addEventListener("click", () => { state.error = ""; render(); }));
  document.querySelectorAll("form[data-action='login']").forEach((form) => form.addEventListener("submit", async (event) => {
    event.preventDefault();
    const button = form.querySelector("button"); button.disabled = true;
    try { const body = await api.login(field(form, "username").value, field(form, "password").value); state.user = body.user; state.error = ""; render(); }
    catch (error) { state.error = error.message; render(); }
  }));
  document.querySelectorAll("form[data-action='setup']").forEach((form) => form.addEventListener("submit", async (event) => {
    event.preventDefault();
    const button = form.querySelector("button"); button.disabled = true;
    try {
      const data = { username: field(form, "username").value, displayName: field(form, "displayName").value, password: field(form, "password").value };
      const tmdbToken = field(form, "tmdbToken").value.trim();
      const libraryName = field(form, "libraryName").value.trim();
      if (tmdbToken) data.tmdbToken = tmdbToken;
      if (libraryName) data.firstLibrary = { name: libraryName, kind: field(form, "libraryKind").value, rootPath: field(form, "libraryRoot").value.trim() || undefined };
      await api.setup(data);
      state.initialized = true; state.error = ""; state.setupNotice = "初始化完成，请使用刚创建的管理员登录。"; render();
    } catch (error) { state.error = error.message; render(); }
  }));
  document.querySelectorAll("form[data-action='create-user']").forEach((form) => form.addEventListener("submit", async (event) => {
    event.preventDefault();
    try { await api.createUser({ username: field(form, "username").value, displayName: field(form, "displayName").value, password: field(form, "password").value, isAdmin: field(form, "isAdmin").checked }); state.route = "admin"; state.error = ""; state.notice = "用户已创建。"; render(); }
    catch (error) { state.error = error.message; render(); }
  }));
  document.querySelectorAll("form[data-action='create-library']").forEach((form) => form.addEventListener("submit", async (event) => {
    event.preventDefault();
    try { await api.createLibrary({ name: field(form, "name").value, kind: field(form, "kind").value, realtimeWatchEnabled: field(form, "realtimeWatchEnabled").checked }); state.route = "admin"; state.error = ""; state.notice = "媒体库已创建。"; render(); }
    catch (error) { state.error = error.message; state.notice = ""; render(); }
  }));
  document.querySelectorAll("form[data-action='add-root']").forEach((form) => form.addEventListener("submit", async (event) => {
    event.preventDefault();
    try { await api.addLibraryRoot(form.dataset.libraryId, field(form, "path").value); state.route = "admin"; state.error = ""; state.notice = "根路径已添加。"; render(); }
    catch (error) { state.error = error.message; state.notice = ""; render(); }
  }));
  document.querySelectorAll("form[data-action='update-library']").forEach((form) => form.addEventListener("submit", async (event) => {
    event.preventDefault();
    const optionalNumber = (value) => value ? Number(value) : undefined;
    try {
      await api.updateLibrary(form.dataset.libraryId, { isEnabled: field(form, "isEnabled").checked, realtimeWatchEnabled: field(form, "realtimeWatchEnabled").checked, incrementalSchedule: field(form, "incrementalSchedule").value || null, reconciliationSchedule: field(form, "reconciliationSchedule").value || null, metadataSchedule: field(form, "metadataSchedule").value || null, scanConcurrency: optionalNumber(field(form, "scanConcurrency").value), probeConcurrency: optionalNumber(field(form, "probeConcurrency").value) });
      state.route = "admin"; state.error = ""; state.notice = "媒体库计划已保存。"; render();
    } catch (error) { state.error = error.message; state.notice = ""; render(); }
  }));
  document.querySelectorAll("form[data-action='update-user']").forEach((form) => form.addEventListener("submit", async (event) => {
    event.preventDefault();
    const payload = { displayName: field(form, "displayName").value, isDisabled: field(form, "isDisabled").checked, isAdmin: field(form, "isAdmin").checked, canManageServer: field(form, "canManageServer").checked, canRemoteAccess: field(form, "canRemoteAccess").checked, canDownload: field(form, "canDownload").checked };
    if (field(form, "password").value) payload.password = field(form, "password").value;
    try {
      await api.updateUser(form.dataset.userId, payload);
      const selectedLibraries = new Set(Array.from(form.querySelectorAll("input[name='libraryAccess']:checked"), (input) => input.value));
      const libraries = state.admin?.libraries || [];
      await Promise.all(libraries.map((library) => api.setLibraryAccess(form.dataset.userId, library.id, selectedLibraries.has(library.id))));
      state.route = "admin"; state.error = ""; state.notice = "用户权限已保存。"; render();
    } catch (error) { state.error = error.message; state.notice = ""; render(); }
  }));
  document.querySelectorAll("form[data-action='search']").forEach((form) => form.addEventListener("submit", (event) => {
    event.preventDefault(); const query = field(form, "q").value.trim(); if (!query) return;
    state.query = query; state.route = "search"; state.error = ""; state.notice = ""; render();
  }));
}

async function boot() {
  try {
    state.initialized = (await api.setupStatus()).initialized;
    if (state.initialized && readCookie("lux_csrf")) state.user = (await api.me()).user;
  } catch { state.user = null; }
  render();
}
boot();
