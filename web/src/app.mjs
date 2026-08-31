import {
  clearCsrfToken,
  readCsrfToken,
  rememberCsrfToken,
  requestOptions,
} from "./request-options.mjs";
import { ADMIN_NAV_ITEMS, adminSectionForRoute, isAdminRoute, renderAdminNavigation } from "./admin-navigation.mjs";

const app = document.querySelector("#app");
const state = {
  user: null,
  initialized: true,
  libraries: [],
  home: null,
  admin: null,
  route: "home",
  adminNavExpanded: false,
  libraryId: "",
  libraryFilters: {},
  libraryPage: 1,
  searchPage: 1,
  favoritesPage: 1,
  item: null,
  itemImages: [],
  itemCandidates: [],
  playback: null,
  children: null,
  error: "",
  notice: "",
  networkProxyDiagnostics: null,
  setupNotice: "",
  drawerOpen: false,
};

const api = {
  async request(path, options = {}) {
    const response = await fetch(path, requestOptions(options));
    if (response.status === 204) return null;
    const body = await response.json().catch(() => ({}));
    if (!response.ok) throw new Error(body?.error?.message || "请求失败");
    return body;
  },
  async login(username, password) {
    const response = await this.request("/api/v1/auth/login", { method: "POST", body: JSON.stringify({ username, password }) });
    rememberCsrfToken(response?.csrfToken);
    return response;
  },
  setup(data) { return this.request("/api/v1/setup/complete", { method: "POST", body: JSON.stringify(data) }); },
  async logout() {
    await this.request("/api/v1/auth/logout", { method: "POST", headers: { "x-csrf-token": readCsrfToken() } });
    clearCsrfToken();
  },
  setupStatus() { return this.request("/api/v1/setup/status"); },
  me() { return this.request("/api/v1/auth/me"); },
  sessions() { return this.request("/api/v1/auth/sessions"); },
  revokeSession(id) { return this.request("/api/v1/auth/sessions/" + encodeURIComponent(id), { method: "DELETE", headers: { "x-csrf-token": readCsrfToken() } }); },
  home() { return this.request("/api/v1/home"); },
  favorites(page = 1) { return this.request("/api/v1/favorites?page=" + encodeURIComponent(page) + "&pageSize=24"); },
  libraries() { return this.request("/api/v1/libraries"); },
  libraryItems(id, filters = {}) {
    const params = new URLSearchParams({ page: String(filters.page || 1), pageSize: String(filters.pageSize || 24) });
    Object.entries(filters).forEach(([key, value]) => {
      if (!["page", "pageSize"].includes(key) && value !== "" && value !== null && value !== undefined) params.set(key, String(value));
    });
    return this.request("/api/v1/libraries/" + encodeURIComponent(id) + "/items?" + params.toString());
  },
  item(id) { return this.request("/api/v1/items/" + encodeURIComponent(id)); },
  children(id, filters = {}) {
    const params = new URLSearchParams({ page: "1", pageSize: "60" });
    Object.entries(filters).forEach(([key, value]) => {
      if (value !== "" && value !== null && value !== undefined) params.set(key, String(value));
    });
    return this.request("/api/v1/items/" + encodeURIComponent(id) + "/children?" + params.toString());
  },
  playback(id) { return this.request("/api/v1/items/" + encodeURIComponent(id) + "/playback"); },
  favorite(id, favorite) { return this.request("/api/v1/items/" + encodeURIComponent(id) + "/favorite", { method: "PUT", headers: { "x-csrf-token": readCsrfToken() }, body: JSON.stringify({ favorite }) }); },
  played(id, played) { return this.request("/api/v1/items/" + encodeURIComponent(id) + "/played", { method: "PUT", headers: { "x-csrf-token": readCsrfToken() }, body: JSON.stringify({ played }) }); },
  search(query, page = 1) { return this.request("/api/v1/search?q=" + encodeURIComponent(query) + "&page=" + encodeURIComponent(page) + "&pageSize=24"); },
  adminUsers() { return this.request("/api/v1/admin/users"); },
  createUser(data) { return this.request("/api/v1/admin/users", { method: "POST", headers: { "x-csrf-token": readCsrfToken() }, body: JSON.stringify(data) }); },
  disableUser(id) { return this.request("/api/v1/admin/users/" + encodeURIComponent(id), { method: "DELETE", headers: { "x-csrf-token": readCsrfToken() } }); },
  updateUser(id, data) { return this.request("/api/v1/admin/users/" + encodeURIComponent(id), { method: "PATCH", headers: { "x-csrf-token": readCsrfToken() }, body: JSON.stringify(data) }); },
  userLibraryAccess(id) { return this.request("/api/v1/admin/users/" + encodeURIComponent(id) + "/libraries"); },
  setLibraryAccess(userId, libraryId, canView) { return this.request("/api/v1/admin/users/" + encodeURIComponent(userId) + "/libraries/" + encodeURIComponent(libraryId), { method: "PATCH", headers: { "x-csrf-token": readCsrfToken() }, body: JSON.stringify({ canView }) }); },
  adminLibraries() { return this.request("/api/v1/admin/libraries"); },
  createLibrary(data) { return this.request("/api/v1/admin/libraries", { method: "POST", headers: { "x-csrf-token": readCsrfToken() }, body: JSON.stringify(data) }); },
  updateLibrary(id, data) { return this.request("/api/v1/admin/libraries/" + encodeURIComponent(id), { method: "PATCH", headers: { "x-csrf-token": readCsrfToken() }, body: JSON.stringify(data) }); },
  deleteLibrary(id) { return this.request("/api/v1/admin/libraries/" + encodeURIComponent(id), { method: "DELETE", headers: { "x-csrf-token": readCsrfToken() } }); },
  addLibraryRoot(id, path) { return this.request("/api/v1/admin/libraries/" + encodeURIComponent(id) + "/roots", { method: "POST", headers: { "x-csrf-token": readCsrfToken() }, body: JSON.stringify({ path }) }); },
  deleteLibraryRoot(libraryId, rootId) { return this.request("/api/v1/admin/libraries/" + encodeURIComponent(libraryId) + "/roots/" + encodeURIComponent(rootId), { method: "DELETE", headers: { "x-csrf-token": readCsrfToken() } }); },
  scanLibrary(id) { return this.request("/api/v1/admin/libraries/" + encodeURIComponent(id) + "/scan", { method: "POST", headers: { "x-csrf-token": readCsrfToken() } }); },
  adminJobs(status = "") { return this.request("/api/v1/admin/jobs?page=1&pageSize=50" + (status ? "&status=" + encodeURIComponent(status) : "")); },
  adminJob(id) { return this.request("/api/v1/admin/jobs/" + encodeURIComponent(id)); },
  adminJobEvents(id, filters = {}) {
    const params = new URLSearchParams({ page: String(filters.page || 1), pageSize: "100" });
    if (filters.level) params.set("level", filters.level);
    if (filters.eventCode) params.set("eventCode", filters.eventCode);
    return this.request("/api/v1/admin/jobs/" + encodeURIComponent(id) + "/events?" + params.toString());
  },
  cancelJob(id) { return this.request("/api/v1/admin/jobs/" + encodeURIComponent(id) + "/cancel", { method: "POST", headers: { "x-csrf-token": readCsrfToken() } }); },
  retryJob(id) { return this.request("/api/v1/admin/jobs/" + encodeURIComponent(id) + "/retry", { method: "POST", headers: { "x-csrf-token": readCsrfToken() } }); },
  audit() { return this.request("/api/v1/admin/audit?page=1&pageSize=50"); },
  adminSettings() { return this.request("/api/v1/admin/settings"); },
  updateSettings(data) { return this.request("/api/v1/admin/settings", { method: "PATCH", headers: { "x-csrf-token": readCsrfToken() }, body: JSON.stringify(data) }); },
  testNetworkProxy(networkProxyUrl) { return this.request("/api/v1/admin/settings/network-proxy/test", { method: "POST", headers: { "x-csrf-token": readCsrfToken() }, body: JSON.stringify(networkProxyUrl ? { networkProxyUrl } : {}) }); },
  logs() { return this.request("/api/v1/admin/logs?page=1&pageSize=50"); },
  startBatchReidentify(itemIds) { return this.request("/api/v1/admin/metadata/reidentify", { method: "POST", headers: { "x-csrf-token": readCsrfToken() }, body: JSON.stringify({ itemIds }) }); },
  metadataReidentifyJob(id) { return this.request("/api/v1/admin/metadata/reidentify/" + encodeURIComponent(id)); },
  retryMetadataReidentify(id) { return this.request("/api/v1/admin/metadata/reidentify/" + encodeURIComponent(id), { method: "POST", headers: { "x-csrf-token": readCsrfToken() } }); },
  adminCandidates(itemId) { return this.request("/api/v1/admin/items/" + encodeURIComponent(itemId) + "/identify/candidates?page=1&pageSize=50"); },
  searchCandidates(itemId, query, year) { return this.request("/api/v1/admin/items/" + encodeURIComponent(itemId) + "/identify/candidates", { method: "POST", headers: { "x-csrf-token": readCsrfToken() }, body: JSON.stringify({ query, year: year || undefined }) }); },
  selectCandidate(itemId, candidateId, mode) { return this.request("/api/v1/admin/items/" + encodeURIComponent(itemId) + "/identify/candidates/" + encodeURIComponent(candidateId) + "/select", { method: "POST", headers: { "x-csrf-token": readCsrfToken() }, body: JSON.stringify({ mode }) }); },
  adminImages(itemId) { return this.request("/api/v1/admin/items/" + encodeURIComponent(itemId) + "/images"); },
  deleteAdminImage(itemId, imageId) { return this.request("/api/v1/admin/items/" + encodeURIComponent(itemId) + "/images/" + encodeURIComponent(imageId), { method: "DELETE", headers: { "x-csrf-token": readCsrfToken() } }); },
  adminHealth() { return this.request("/api/v1/admin/health"); },
  ready() { return fetch("/health/ready", { credentials: "same-origin" }).then((response) => response.json()); },
  progress(id, positionTicks, durationTicks) { return this.request("/api/v1/items/" + encodeURIComponent(id) + "/progress", { method: "POST", headers: { "x-csrf-token": readCsrfToken() }, body: JSON.stringify({ positionTicks, durationTicks }) }); },
};

function field(form, name) {
  return form.elements.namedItem(name);
}

function escapeHtml(value) {
  return String(value ?? "").replace(/[&<>"']/g, (character) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[character]);
}

function brandLogo(className, alt = "") {
  const decorative = alt ? "" : " aria-hidden=\"true\"";
  return `<img class="${className}" src="/logo.svg" alt="${escapeHtml(alt)}" width="306" height="346"${decorative} decoding="async">`;
}

function imageUrl(item, imageType = "poster") {
  return item?.imageTags?.[imageType] ? "/api/v1/items/" + encodeURIComponent(item.id) + "/images/" + imageType : "";
}

/**
 * Emby & Jellyfin Style Poster Component with Graceful Gradient Fallbacks
 */
function poster(item, className = "poster", isLandscape = false) {
  const url = imageUrl(item, isLandscape ? "fanart" : "poster") || imageUrl(item, "poster");
  const title = item.title || item.name || "未命名媒体";
  const icon = item.itemType === "SERIES" || item.type === "SERIES" ? "📺" : "🎬";
  const year = item.productionYear ? ` (${item.productionYear})` : "";
  
  if (url) {
    return `<div class="poster-container"><img class="${className}" src="${url}" alt="${escapeHtml(title)}" loading="lazy"><div class="poster-overlay"><button class="poster-play-btn" type="button" aria-label="播放 ${escapeHtml(title)}">▶</button></div></div>`;
  }

  // Jellyfin/Emby styled placeholder card when image is missing
  return `<div class="${className} poster-placeholder" role="img" aria-label="${escapeHtml(title)}">
    <div class="poster-placeholder-content">
      <span class="poster-icon">${icon}</span>
      <span class="poster-title-text">${escapeHtml(title)}</span>
      <span class="poster-year-text">${escapeHtml(year)}</span>
    </div>
    <div class="poster-overlay"><button class="poster-play-btn" type="button" aria-label="播放 ${escapeHtml(title)}">▶</button></div>
  </div>`;
}

function loading() { return "<div class=\"loading\" aria-busy=\"true\"><div class=\"spinner\"></div><span>正在加载媒体库...</span></div>"; }

function titleForRoute() {
  if (state.route === "libraries") return "我的媒体库";
  if (state.route === "favorites") return "我的收藏";
  if (state.route === "library") return state.libraries.find((library) => library.id === state.libraryId)?.name || "媒体库内容";
  if (state.route === "search") return "搜索结果";
  if (state.route === "item") return state.item?.title || "媒体详情";
  if (isAdminRoute(state.route)) return ADMIN_NAV_ITEMS.find((item) => item.id === adminSectionForRoute(state.route))?.label || "管理控制台";
  if (state.route === "account") return "账户与会话";
  return "首页";
}

function render() {
  if (state.initialized === false) return renderSetup();
  if (!state.user) return renderAuth();
  const error = state.error ? "<div class=\"notice error\" role=\"alert\">" + escapeHtml(state.error) + "</div>" : "";
  const notice = state.notice ? "<div class=\"notice\" role=\"status\">" + escapeHtml(state.notice) + "</div>" : "";
  const drawerClass = state.drawerOpen ? " drawer-is-open" : "";
  app.innerHTML = "<div class=\"shell" + drawerClass + "\"><aside class=\"sidebar\">" + brand() + nav() + account() + "</aside><button class=\"drawer-scrim\" type=\"button\" data-action=\"close-drawer\" aria-label=\"关闭导航\"></button><div class=\"app-frame\"><header class=\"app-toolbar\"><button class=\"toolbar-menu\" type=\"button\" data-action=\"toggle-drawer\" aria-label=\"打开导航\">☰</button><button class=\"toolbar-brand\" type=\"button\" data-route=\"home\">" + brandLogo("brand-logo") + "<span>Lux</span></button><div class=\"toolbar-title\"><h1>" + titleForRoute() + "</h1></div><form class=\"search-form\" data-action=\"search\"><input class=\"search-box\" name=\"q\" type=\"search\" placeholder=\"🔍 搜索电影、剧集、演职人员...\" aria-label=\"搜索\"></form><button class=\"toolbar-user\" type=\"button\" data-route=\"account\" aria-label=\"打开账户\"><span class=\"user-avatar\">" + escapeHtml((state.user.displayName || state.user.usernameNormalized || "L").slice(0, 1).toUpperCase()) + "</span><span class=\"toolbar-user-name\">" + escapeHtml(state.user.displayName || state.user.usernameNormalized) + "</span></button></header><main id=\"main-content\" class=\"content\">" + error + notice + "<section id=\"view\">" + loading() + "</section></main></div></div>";
  bind();
  loadRoute();
}

function brand() { return "<div class=\"brand\"><button class=\"brand-home\" type=\"button\" data-route=\"home\">" + brandLogo("brand-logo") + "<span><strong>Lux</strong></span></button><button class=\"drawer-close\" type=\"button\" data-action=\"close-drawer\" aria-label=\"关闭导航\">×</button></div>"; }

function nav() {
  const homeCurrent = state.route === "home" ? "page" : "false";
  const libraryCurrent = state.route === "libraries" ? "page" : "false";
  const favoritesCurrent = state.route === "favorites" ? "page" : "false";
  const adminExpanded = state.adminNavExpanded && isAdminRoute(state.route);
  const admin = state.user.canManageServer ? renderAdminNavigation({ expanded: adminExpanded, route: state.route }) : "";
  const libraries = accessibleLibraries();
  const libraryLinks = libraries.length ? "<div class=\"nav-group\"><span class=\"nav-group-label\">我的媒体库</span>" + libraries.map((library) => "<button class=\"nav-library\" data-library=\"" + escapeHtml(library.id) + "\" aria-current=\"" + (state.route === "library" && state.libraryId === library.id ? "page" : "false") + "\"><span class=\"nav-glyph\">" + (library.kind === "SERIES" ? "📺" : library.kind === "MOVIE" ? "🎬" : "📁") + "</span><span>" + escapeHtml(library.name) + "</span></button>").join("") + "</div>" : "";
  return "<nav class=\"nav\" aria-label=\"主导航\"><button data-route=\"home\" aria-current=\"" + homeCurrent + "\"><span class=\"nav-glyph\">🏠</span><span>首页</span></button><button data-route=\"libraries\" aria-current=\"" + libraryCurrent + "\"><span class=\"nav-glyph\">🍿</span><span>媒体库</span></button><button data-route=\"favorites\" aria-current=\"" + favoritesCurrent + "\"><span class=\"nav-glyph\">❤️</span><span>收藏</span></button><button data-route=\"account\" aria-current=\"" + (state.route === "account" ? "page" : "false") + "\"><span class=\"nav-glyph\">👤</span><span>账户</span></button>" + admin + libraryLinks + "</nav>";
}

function account() {
  return "<div class=\"sidebar-footer\"><span class=\"sidebar-label\">当前登录</span><strong>" + escapeHtml(state.user.displayName || state.user.usernameNormalized) + "</strong><button class=\"button secondary sidebar-action\" data-route=\"account\">账户设置</button><button class=\"button secondary sidebar-action\" data-action=\"logout\">退出登录</button></div>";
}

function renderAuth() {
  const error = state.error ? "<div class=\"notice error\" role=\"alert\">" + escapeHtml(state.error) + "</div>" : "";
  const notice = state.setupNotice ? "<div class=\"notice\" role=\"status\">" + escapeHtml(state.setupNotice) + "</div>" : "";
  app.innerHTML = "<div class=\"auth-layout\"><section class=\"auth-card\"><div class=\"auth-brand\">" + brandLogo("auth-logo", "Lux") + "</div><h1 style=\"margin-top:.7rem\">欢迎使用 Lux</h1><p>连接并享受你的私人高画质影院库。</p>" + notice + error + "<form data-action=\"login\"><div class=\"field\"><label for=\"username\">用户名</label><input id=\"username\" name=\"username\" autocomplete=\"username\" placeholder=\"输入用户名\" required></div><div class=\"field\"><label for=\"password\">密码</label><input id=\"password\" name=\"password\" type=\"password\" autocomplete=\"current-password\" placeholder=\"输入密码\" required></div><div class=\"form-actions\"><button class=\"button\" type=\"submit\">登 录</button></div></form></section></div>";
  bind();
}

function renderAccount(sessions) {
  const rows = sessions.map((session) => `<tr><td>${session.isCurrent ? "当前设备" : "其他浏览器"}</td><td>${escapeHtml(new Date((session.updatedAt || session.createdAt) * 1000).toLocaleString())}</td><td>${session.isCurrent ? "<span class=\"badge badge-emerald\">活跃中</span>" : `<button class="button secondary" type="button" data-revoke-session="${escapeHtml(session.id)}">撤销</button>`}</td></tr>`).join("");
  return `<section class="section"><div class="section-heading"><h2>账户与会话管理</h2><span>${escapeHtml(state.user.displayName || state.user.usernameNormalized)}</span></div><p>当前账号关联的设备活跃记录：</p><div class="table-wrap"><table><thead><tr><th>设备</th><th>最近活动</th><th>状态</th></tr></thead><tbody>${rows || "<tr><td colspan=\"3\">暂无会话记录</td></tr>"}</tbody></table></div></section>`;
}

function renderSetup() {
  const error = state.error ? "<div class=\"notice error\" role=\"alert\">" + escapeHtml(state.error) + "</div>" : "";
  app.innerHTML = "<div class=\"auth-layout\"><section class=\"auth-card\"><div class=\"auth-brand\">" + brandLogo("auth-logo", "Lux") + "</div><h1 style=\"margin-top:.7rem\">初始化 Lux 媒体服务器</h1><p>创建首个服务器超级管理员。媒体库和刮削配置可在稍后配置。</p>" + error + "<form data-action=\"setup\"><div class=\"field\"><label for=\"setup-username\">管理员用户名</label><input id=\"setup-username\" name=\"username\" autocomplete=\"username\" required></div><div class=\"field\"><label for=\"setup-display-name\">显示名称</label><input id=\"setup-display-name\" name=\"displayName\" autocomplete=\"name\"></div><div class=\"field\"><label for=\"setup-password\">管理员密码</label><input id=\"setup-password\" name=\"password\" type=\"password\" autocomplete=\"new-password\" minlength=\"8\" required></div><fieldset class=\"setup-options\"><legend>可选配置</legend><div class=\"field\"><label for=\"setup-library-name\">首个媒体库名称</label><input id=\"setup-library-name\" name=\"libraryName\" placeholder=\"例: 电影库\"></div><div class=\"field\"><label for=\"setup-library-kind\">媒体库类型</label><select id=\"setup-library-kind\" name=\"libraryKind\"><option value=\"MIXED\">混合</option><option value=\"MOVIE\">电影</option><option value=\"SERIES\">剧集</option></select></div><div class=\"field\"><label for=\"setup-library-root\">媒体库路径 (NAS / 挂载点)</label><input id=\"setup-library-root\" name=\"libraryRoot\" placeholder=\"例如 /media/movies\"><small>服务端将自动扫描改目录下所有的视频文件与 NFO。</small></div></fieldset><div class=\"form-actions\"><button class=\"button\" type=\"submit\">完成配置</button></div></form></section></div>";
  bind();
}

async function loadRoute() {
  const view = document.querySelector("#view");
  if (!view) return;
  try {
    if (state.route === "home") {
      state.home = await api.home();
      const homeLibraries = Array.isArray(state.home?.libraries) ? state.home.libraries : [];
      state.libraries = homeLibraries.length ? homeLibraries : ((await api.libraries()).libraries || []);
      view.innerHTML = renderHome();
    } else if (state.route === "libraries") {
      state.libraries = state.libraries.length ? state.libraries : (await api.libraries()).libraries || [];
      view.innerHTML = renderLibraries();
    } else if (state.route === "favorites") {
      const result = await api.favorites(state.favoritesPage);
      view.innerHTML = renderFavorites(result);
    } else if (state.route === "library") {
      state.libraries = state.libraries.length ? state.libraries : (await api.libraries()).libraries || [];
      const result = await api.libraryItems(state.libraryId, { ...state.libraryFilters, page: state.libraryPage });
      const library = state.libraries.find((entry) => entry.id === state.libraryId);
      view.innerHTML = renderLibraryItems(result, library);
    } else if (state.route === "search") {
      const result = await api.search(state.query, state.searchPage);
      view.innerHTML = "<section class=\"section\">" + renderGrid(result.items || [], "搜索“" + escapeHtml(state.query) + "”") + renderPagination(result, "search-page", "搜索结果页") + "</section>";
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
    } else if (isAdminRoute(state.route)) {
      const [health, libraries, users, audit, logs, settings, pending, jobs] = await Promise.all([
        api.adminHealth(), api.adminLibraries(), api.adminUsers(), api.audit(), api.logs(), api.adminSettings(), api.pendingMetadata(), api.adminJobs()
      ]);
      const accessEntries = await Promise.all((users.users || []).map(async (user) => [user.id, (await api.userLibraryAccess(user.id)).libraryIds || []]));
      state.admin = { health, ready: health, settings, libraries: libraries.libraries || [], users: users.users || [], audit: audit.events || [], logs: logs.events || [], pending: pending.items || [], jobs: jobs.jobs || [], access: Object.fromEntries(accessEntries), reidentifyJobs: state.admin?.reidentifyJobs || [], jobDetail: state.admin?.jobDetail || null, jobEvents: state.admin?.jobEvents || [], jobEventsTotal: state.admin?.jobEventsTotal || 0, jobEventsPage: state.admin?.jobEventsPage || 1, jobEventFilters: state.admin?.jobEventFilters || {} };
      view.innerHTML = renderAdmin();
    } else if (state.route === "account") {
      const sessions = await api.sessions();
      view.innerHTML = renderAccount(sessions.sessions || []);
    }
    bind();
  } catch (error) {
    state.error = error.message;
    view.innerHTML = "<div class=\"empty\"><h2>暂时无法加载内容</h2><p>" + escapeHtml(error.message) + "</p><button class=\"button secondary\" data-action=\"retry\">重新加载</button></div>";
    bind();
  }
}

/**
 * Emby & Jellyfin Layout Home Screen
 */
function renderHome() {
  const continueWatching = state.home?.continueWatching || [];
  const libraries = accessibleLibraries();
  const featured = continueWatching[0];
  const backdropUrl = featured ? (imageUrl(featured, "fanart") || imageUrl(featured)) : "";
  const backdrop = backdropUrl ? `<img class="home-hero-backdrop" src="${backdropUrl}" alt="">` : "<div class=\"home-hero-backdrop\"></div>";
  
  const hero = featured
    ? `<article class="home-hero">${backdrop}<div class="home-hero-content">
        <div class="hero-badges"><span class="badge badge-indigo">4K HDR</span><span class="badge badge-emerald">Dolby Atmos</span></div>
        <h2>${escapeHtml(featured.title || featured.name)}</h2>
        <p>${escapeHtml(featured.overview || "从上次停下的位置继续观看精彩内容。")}</p>
        <div class="hero-actions">
          <button class="button" type="button" data-item="${escapeHtml(featured.id)}">▶ 立即播放</button>
          <button class="button secondary" type="button" data-item="${escapeHtml(featured.id)}">ℹ 详细信息</button>
          <span class="hero-meta">${escapeHtml([featured.productionYear || "", featured.itemType === "SERIES" ? "剧集" : "电影"].filter(Boolean).join(" · "))}</span>
        </div>
      </div></article>`
    : `<article class="home-hero home-hero-empty"><div class="home-hero-content">
        <h2>你的私人高清影院</h2>
        <p>支持 4K 原盘直放、本地 NFO 元数据刮削与多设备同步。</p>
        <div class="hero-actions"><button class="button" type="button" data-route="libraries">浏览我的媒体库</button></div>
      </div></article>`;

  const libCards = libraries.length
    ? libraries.map(libraryCard).join("")
    : "<div class=\"empty\"><h3>暂无可见媒体库</h3><p>请在控制台中创建媒体库并绑定本地挂载路径。</p></div>";

  const myLibrariesSection = `<section class="section home-section"><div class="section-heading"><h2>🍿 我的媒体库</h2><span>${libraries.length} 个本地存储卷</span></div><div class="library-rail">${libCards}</div></section>`;
  
  const progress = continueWatching.length
    ? `<section class="section home-section"><div class="section-heading"><h2>▶ 继续观看</h2><span>${continueWatching.length} 个未完结项</span></div>${renderRail(continueWatching, true)}</section>`
    : "";

  const latestShelves = libraries
    .filter((library) => Array.isArray(library.latest) && library.latest.length)
    .map((library) => `<section class="section home-section"><div class="section-heading"><h2>最新${escapeHtml(library.name)}</h2><span>${library.latest.length} 项</span></div>${renderRail(library.latest)}</section>`)
    .join("");
    
  return hero + myLibrariesSection + progress + latestShelves;
}

function accessibleLibraries() {
  return state.libraries.length
    ? state.libraries
    : (Array.isArray(state.home?.libraries) ? state.home.libraries : []);
}

function libraryCard(library) {
  const icon = library.kind === "SERIES" ? "📺" : library.kind === "MOVIE" ? "🎬" : "📁";
  const cover = library.coverImageUrl
    ? `<img class="library-card-cover" src="${escapeHtml(library.coverImageUrl)}" alt="" loading="lazy">`
    : `<span class="library-card-cover library-card-cover-empty" aria-hidden="true">${icon}</span>`;
  return `<button class="library-card" data-library="${escapeHtml(library.id)}">
    <span class="library-card-cover-wrap">${cover}</span>
    <span class="library-card-info"><strong>${escapeHtml(library.name)}</strong></span>
  </button>`;
}

function renderLibraries() {
  const content = state.libraries.length ? state.libraries.map(libraryCard).join("") : "<div class=\"empty\"><h2>暂无媒体库</h2><p>当前账号未分配可访问的媒体库。</p></div>";
  return "<section class=\"section\"><div class=\"library-grid\">" + content + "</div></section>";
}

function renderFavorites(result) {
  return "<section class=\"section\"><div class=\"library-header\"><div><h2>我的收藏</h2></div><span>共 " + (result.total || 0) + " 项</span></div>" + renderGrid(result.items || []) + renderPagination(result, "favorites-page", "收藏翻页") + "</section>";
}

function renderLibraryItems(result, library) {
  const filters = state.libraryFilters || {};
  const selected = (name, value) => filters[name] === value ? " selected" : "";
  const form = `<form class="filter-form" data-action="library-filter">
    <label>类型 <select name="item_type"><option value="">全部类型</option><option value="movie"${selected("item_type", "movie")}>电影</option><option value="series"${selected("item_type", "series")}>剧集</option><option value="episode"${selected("item_type", "episode")}>单集</option></select></label>
    <label>年份 <input name="year" type="number" min="1800" max="2200" value="${escapeHtml(filters.year || "")}" placeholder="例: 2024"></label>
    <label>观看状态 <select name="is_played"><option value="">全部状态</option><option value="true"${selected("is_played", "true")}>已看</option><option value="false"${selected("is_played", "false")}>未看</option></select></label>
    <label>收藏 <select name="is_favorite"><option value="">全部</option><option value="true"${selected("is_favorite", "true")}>已收藏</option><option value="false"${selected("is_favorite", "false")}>未收藏</option></select></label>
    <label>排序 <select name="sort_by"><option value="">名称</option><option value="DateCreated"${selected("sort_by", "DateCreated")}>最近添加</option></select></label>
    <label>顺序 <select name="sort_order"><option value="Ascending"${selected("sort_order", "Ascending")}>正序</option><option value="Descending"${selected("sort_order", "Descending")}>倒序</option></select></label>
    <button class="button" type="submit">筛选</button>
    <button class="button secondary" type="button" data-action="clear-library-filter">重置</button>
  </form>`;
  return "<section class=\"section\"><div class=\"library-header\"><div><h2>" + escapeHtml(library?.name || "媒体库") + "</h2></div><span>共 " + (result.total || 0) + " 项</span></div><div class=\"library-toolbar\">" + form + "</div>" + renderGrid(result.items || []) + renderPagination(result, "library-page", "媒体库翻页") + "</section>";
}

function renderGrid(items, heading = "") {
  const title = heading ? "<div class=\"section-heading\" style=\"grid-column:1/-1\"><h2>" + heading + "</h2><span>" + items.length + " 项</span></div>" : "";
  const content = items.length ? items.map((item) => mediaCard(item)).join("") : "<div class=\"empty\" style=\"grid-column:1/-1\"><h3>没有符合条件的内容</h3><p>请尝试重置筛选或更改搜索词。</p></div>";
  return "<div class=\"media-grid\">" + title + content + "</div>";
}

function renderRail(items, isLandscape = false) {
  const content = items.length ? items.map((item) => mediaCard(item, isLandscape)).join("") : "<div class=\"empty\"><h3>暂无动态</h3></div>";
  return "<div class=\"media-rail\">" + content + "</div>";
}

function renderPagination(result, action, label) {
  const page = Number(result.page || 1);
  const pageSize = Number(result.pageSize || 24);
  const total = Number(result.total || 0);
  const pageCount = Math.max(1, Math.ceil(total / pageSize));
  if (pageCount <= 1) return "";
  return `<nav class="pagination" aria-label="${escapeHtml(label)}"><button class="button secondary" type="button" data-${action}="${page - 1}"${page <= 1 ? " disabled" : ""}>上一页</button><span>第 ${page} / ${pageCount} 页</span><button class="button secondary" type="button" data-${action}="${page + 1}"${page >= pageCount ? " disabled" : ""}>下一页</button></nav>`;
}

/**
 * Emby & Jellyfin Style Media Poster Card
 */
function mediaCard(item, isLandscape = false) {
  const userData = item.userData || {};
  const isPlayed = userData.isPlayed;
  const isFavorite = userData.isFavorite;
  const positionTicks = userData.playbackPositionTicks || 0;
  const durationTicks = item.runtimeTicks || 0;
  const progressPercent = (durationTicks && positionTicks) ? Math.min(100, Math.round((positionTicks / durationTicks) * 100)) : 0;
  
  const playedBadge = isPlayed ? `<span class="card-badge-played">✓</span>` : "";
  const favBadge = isFavorite ? `<span class="card-badge-fav">❤️</span>` : "";
  const progressBar = (progressPercent > 0 && progressPercent < 90) ? `<div class="card-progress-bar"><div class="card-progress-fill" style="width:${progressPercent}%"></div></div>` : "";
  const posterClass = isLandscape ? "poster poster-landscape" : "poster";

  return `<button class="media-card${isLandscape ? " media-card-landscape" : ""}" data-item="${escapeHtml(item.id)}">
    ${poster(item, posterClass, isLandscape)}
    ${playedBadge}
    ${favBadge}
    ${progressBar}
    <span class="media-card-body">
      <strong>${escapeHtml(item.title || item.name)}</strong>
      <span class="media-meta">${escapeHtml([item.productionYear || (item.itemType === "SERIES" ? "剧集" : "电影"), isPlayed ? "已看" : ""].filter(Boolean).join(" · "))}</span>
    </span>
  </button>`;
}

function formatRuntime(ticks) {
  const minutes = Math.round(Number(ticks || 0) / 10000000 / 60);
  if (!minutes) return "";
  const hours = Math.floor(minutes / 60);
  return hours ? hours + " 小时 " + (minutes % 60) + " 分钟" : minutes + " 分钟";
}

/**
 * Emby & Jellyfin Media Detail Layout
 */
function renderDetail(item, playback = {}, children = null, images = [], candidates = []) {
  const sources = item.mediaSources || [];
  const backdropUrl = imageUrl(item, "fanart") || imageUrl(item);
  const backdrop = backdropUrl ? `<img class="detail-backdrop" src="${backdropUrl}" alt="">` : "<div class=\"detail-backdrop\"></div>";
  const chips = sources.map((source) => "<span class=\"badge badge-indigo\">" + escapeHtml(source.qualityLabel || source.editionName || source.container || "DIRECT") + "</span>").join("");
  const runtimeTicks = item.runtimeTicks || sources.find((source) => source.isDefault)?.durationTicks || sources[0]?.durationTicks;
  const subtitleCount = sources.reduce((count, source) => count + (source.streams || []).filter((stream) => stream.type === "SUBTITLE").length, 0);
  const audioCount = sources.reduce((count, source) => count + (source.streams || []).filter((stream) => stream.type === "AUDIO").length, 0);
  
  const detailFacts = [
    [formatRuntime(runtimeTicks), "时长"],
    [sources.length ? sources.length + " 个版本" : "", "媒体源"],
    [subtitleCount ? subtitleCount + " 条字幕" : "无字幕", "外挂/内嵌字幕"],
    [audioCount ? audioCount + " 条音轨" : "主音轨", "音频规格"]
  ].filter(([value]) => value).map(([value, label]) => `<div><strong>${escapeHtml(value)}</strong><span>${escapeHtml(label)}</span></div>`).join("");
  
  const buttons = sources.map((source, index) => "<button class=\"button secondary\" data-source=\"" + escapeHtml(source.id) + "\" aria-pressed=\"" + (index === 0) + "\">" + escapeHtml(source.qualityLabel || source.editionName || source.container || "版本 " + (index + 1)) + "</button>").join("");
  
  const player = sources.length
    ? `<div class="player-container">
        <div class="source-list" aria-label="媒体版本">${buttons}</div>
        <p class="player-status" data-player-status role="status"></p>
        <video class="player" controls preload="metadata" data-player aria-label="播放 ${escapeHtml(item.title || item.name)}" src="/api/v1/items/${encodeURIComponent(item.id)}/stream?sourceId=${encodeURIComponent(sources[0].id)}"></video>
      </div>`
    : "";

  const favoriteLabel = playback.isFavorite ? "❤️ 已收藏" : "🤍 收藏";
  const playedLabel = playback.isPlayed ? "✓ 已看" : "👁️ 标记已看";
  const userData = `<div class="chips"><span class="chip">${playback.isPlayed ? "已观看" : "未观看"}</span>${playback.isFavorite ? '<span class="chip">已加入收藏</span>' : ""}${playback.positionTicks ? '<span class="chip">已播放至 ' + Math.round(playback.positionTicks / 10000000) + ' 秒</span>' : ""}</div>`;
  const childrenPanel = children ? `<section class="children-panel" id="children-panel">${renderChildrenPanel(item, children)}</section>` : "";

  return `<a class="back-link" href="#home" data-route="home">← 返回上一级</a>
  <article class="detail">
    ${backdrop}
    <div class="detail-art">${poster(item, "detail-poster")}</div>
    <div class="detail-copy">
      <span class="eyebrow">${escapeHtml(item.itemType === "SERIES" ? "电视剧" : "电影")}</span>
      <h2>${escapeHtml(item.title || item.name)}</h2>
      <div class="chips">${item.productionYear ? `<span class="badge badge-emerald">${item.productionYear}</span>` : ""}${chips}</div>
      <div class="detail-facts">${detailFacts}</div>
      ${userData}
      <p class="overview">${escapeHtml(item.overview || "暂无该影片的详细剧情介绍。")}</p>
      <div class="form-actions">
        <button class="button secondary" data-action="toggle-favorite" aria-pressed="${Boolean(playback.isFavorite)}">${favoriteLabel}</button>
        <button class="button secondary" data-action="toggle-played" aria-pressed="${Boolean(playback.isPlayed)}">${playedLabel}</button>
      </div>
      ${childrenPanel}
      ${renderAdminImages(item, images)}
      ${renderAdminCandidates(item, candidates)}
      ${player}
    </div>
  </article>`;
}

function renderAdminImages(item, images) {
  if (!state.user?.canManageServer) return "";
  const rows = images.map((image) => {
    const type = image.imageType === "FANART" ? "fanart" : "poster";
    return `<li><img src="/api/v1/items/${encodeURIComponent(item.id)}/images/${type}" alt="${escapeHtml(type)}"><div><strong>${escapeHtml(image.imageType)} #${image.imageIndex}</strong><span>${escapeHtml(image.source || "LOCAL")} · ${escapeHtml(String(image.fileSize || 0))} bytes</span></div><button class="button secondary" type="button" data-delete-image="${escapeHtml(image.id)}">删除</button></li>`;
  }).join("");
  return `<section class="admin-images"><div class="section-heading"><h3>🖼️ 元数据图片管理</h3><span>支持查看与清理本地索引的元数据海报</span></div><ul class="admin-list">${rows || "<li><span>暂无自定义图片</span></li>"}</ul></section>`;
}

function renderAdminCandidates(item, candidates) {
  if (!state.user?.canManageServer) return "";
  const rows = candidates.map((candidate) => {
    const data = candidate.candidate && typeof candidate.candidate === "object" ? candidate.candidate : {};
    const title = data.title || data.originalTitle || candidate.providerId || "未命名候选";
    const diffs = (candidate.fieldDiffs || []).map((diff) => `<li><strong>${escapeHtml(diff.field)}</strong><span>${escapeHtml(String(diff.current ?? "空"))} → ${escapeHtml(String(diff.candidate ?? "空"))}</span></li>`).join("");
    return `<article class="candidate-card"><div class="section-heading"><div><h4>${escapeHtml(title)}</h4><span>${escapeHtml(candidate.provider || "元数据刮削器")} · ID: ${escapeHtml(candidate.providerId || "")}</span></div><span class="badge badge-indigo">匹配度: ${escapeHtml(String(candidate.score ?? 0))}%</span></div><ul class="diff-list">${diffs || "<li><span>无差异</span></li>"}</ul><div class="form-actions"><button class="button secondary" data-select-candidate="${escapeHtml(candidate.itemId)}|${escapeHtml(candidate.id)}|fillMissing">仅补缺</button><button class="button" data-select-candidate="${escapeHtml(candidate.itemId)}|${escapeHtml(candidate.id)}|refreshUnlocked">刷新未锁定</button></div></article>`;
  }).join("");
  return `<section class="admin-images"><div class="section-heading"><h3>🔍 元数据匹配与刮削</h3><span>从当前刮削器查找候选并写回本地 NFO</span></div><form class="admin-form compact-form" data-action="search-candidates"><input name="query" value="${escapeHtml(item.title || "")}" placeholder="输入关键词搜索元数据" aria-label="元数据搜索关键词" required><input name="year" type="number" min="1800" max="2200" placeholder="年份" aria-label="年份"><button class="button secondary" type="submit">搜索候选</button></form><div class="candidate-list">${rows || "<div class=\"empty\"><p>未找到候选。请输入正确的影视名称搜索。</p></div>"}</div></section>`;
}

function renderChildrenPanel(item, result, showingEpisodes = false) {
  const items = result.items || [];
  if (item.itemType === "BOX_SET") return `<div class="section-heading"><h3>合集影片</h3><span>${result.total || items.length} 部</span></div>${renderGrid(items)}`;
  if (showingEpisodes) return `<div class="section-heading"><h3>单集列表</h3><button class="button secondary" type="button" data-show-seasons>返回季度</button></div>${renderGrid(items)}`;
  const seasons = items.map((season) => `<button class="library-card" data-season="${escapeHtml(season.id)}"><strong>${escapeHtml(season.title || season.name)}</strong><span class="media-meta">查看单集 →</span></button>`).join("");
  return `<div class="section-heading"><h3>季度列表</h3><span>${result.total || items.length} 个季度</span></div><div class="library-grid">${seasons || "<div class=\"empty\"><span>暂无季度</span></div>"}</div>`;
}

function renderNetworkProxyDiagnostics(diagnostics) {
  if (!diagnostics) return "";
  const source = diagnostics.proxySource === "input" ? "当前输入地址" : diagnostics.proxySource === "settings" ? "已保存设置" : diagnostics.proxySource === "environment" ? "环境变量" : "当前直连配置";
  const rows = (diagnostics.probes || []).map((probe) => {
    const result = probe.reachable
      ? (probe.latencyMs ?? "—") + " ms" + (probe.status ? " · HTTP " + probe.status : "")
      : "检测失败（" + (probe.error || "请求失败") + "）";
    return "<li><strong>" + escapeHtml(probe.label || probe.id) + "</strong><span>" + escapeHtml(result) + "</span></li>";
  }).join("");
  return "<div class=\"network-proxy-diagnostics\"><div class=\"section-heading\"><h3>网络检测结果</h3><span>来源：" + escapeHtml(source) + "</span></div><ul class=\"admin-list\"><li><strong>网络出口 IP</strong><span>" + escapeHtml(diagnostics.egressIp || "未获取") + "</span></li><li><strong>出口国家/地区</strong><span>" + escapeHtml(diagnostics.egressCountry || "未获取") + "</span></li>" + rows + "</ul><p>延迟为 Lux 服务端发起请求到收到响应的耗时。</p></div>";
}

function renderAdminSettings(settings = {}, diagnostics = null) {
  const networkProxy = settings.networkProxy || {};
  const proxySource = networkProxy.source === "environment" && !networkProxy.url
    ? "当前代理由环境变量提供。"
    : "";
  const credentialNote = networkProxy.hasCredentials
    ? "代理认证信息已配置，页面不会显示密码；保存当前脱敏地址会保留现有认证信息。"
    : "如代理需要认证，可在地址中填写用户名；认证信息只保存在服务器配置文件中。";
  return "<section class=\"section\"><div class=\"section-heading\"><h2>服务端播放设置</h2><span>播放完成度与继续观看阈值</span></div><form class=\"admin-form\" data-action=\"update-settings\"><label>标记已看百分比 <input name=\"resumePlayedPercent\" type=\"number\" min=\"1\" max=\"100\" value=\"" + escapeHtml(settings.resumePlayedPercent ?? 90) + "\" required></label><label>最短进度 (Ticks) <input name=\"resumeMinTicks\" type=\"number\" min=\"0\" value=\"" + escapeHtml(settings.resumeMinTicks ?? 1200000000) + "\" required></label><button class=\"button\" type=\"submit\">保存设置</button></form><p>注：1 秒等于 10,000,000 ticks。</p></section><section class=\"section\"><div class=\"section-heading\"><h2>网络代理设置</h2><span>供 Lux 发出的外部网络请求使用</span></div><form class=\"admin-form\" data-action=\"update-settings\"><label>代理地址 <input name=\"networkProxyUrl\" type=\"url\" aria-label=\"网络代理地址\" autocomplete=\"off\" placeholder=\"http://192.168.1.2:7890\" value=\"" + escapeHtml(networkProxy.url || "") + "\"></label><p>支持 HTTP、HTTPS、SOCKS4、SOCKS4A、SOCKS5 和 SOCKS5H。" + escapeHtml(credentialNote) + "</p>" + (proxySource ? "<p>" + escapeHtml(proxySource) + "</p>" : "") + "<div class=\"form-actions\"><button class=\"button\" type=\"submit\">保存网络代理</button><button class=\"button secondary\" type=\"button\" data-network-proxy-test>检测延迟与出口</button></div></form><p>保存后需要重启 Lux 才会生效。</p>" + renderNetworkProxyDiagnostics(diagnostics) + "</section>";
}

function renderAdminLogs(logs) {
  const rows = logs.slice(0, 12).map((event) => "<li><strong>" + escapeHtml(event.eventType || event.eventCode || "EVENT") + "</strong><span>" + escapeHtml(event.actorUsername || "system") + " · " + escapeHtml(event.createdAt || "") + "</span></li>").join("");
  return "<section class=\"section\"><div class=\"section-heading\"><h2>日志与审计</h2><span>最近系统事件记录</span></div><ul class=\"admin-list\">" + (rows || "<li><span>暂无日志记录</span></li>") + "</ul></section>";
}

function renderAdmin() {
  const admin = state.admin || {};
  const section = adminSectionForRoute(state.route);
  if (section === "libraries") return renderAdminLibraries(admin);
  if (section === "users") return renderAdminUsers(admin);
  if (section === "jobs") return renderAdminJobs(admin);
  if (section === "metadata") return renderAdminMetadata(admin);
  if (section === "settings") return renderAdminSettings(admin.settings, state.networkProxyDiagnostics);
  return renderAdminDashboard(admin);
}

function renderAdminDashboard({ ready = {}, health: reportedHealth, libraries = [], users = [] }) {
  const health = reportedHealth || ready;
  const rootCount = (health.libraries || []).reduce((total, library) => total + Number(library.rootCount || 0), 0);
  const availableRootCount = (health.libraries || []).reduce((total, library) => total + Number(library.availableRootCount || 0), 0);
  const writableRootCount = (health.libraries || []).reduce((total, library) => total + Number(library.writableRootCount || 0), 0);
  
  const healthDetails = `<section class="section">
    <div class="section-heading"><h2>系统状态诊断</h2><span>服务器健康度与路径检测</span></div>
    <div class="admin-list">
      <div><strong>ffprobe 工具</strong><span>${health.ffprobe?.available ? "🟢 可用" : "🔴 未就绪"}</span></div>
      <div><strong>配置卷路径</strong><span>${health.config?.writable ? "🟢 可读写" : "🔴 权限异常"}</span></div>
      <div><strong>媒体挂载根目录</strong><span>${availableRootCount}/${rootCount} 可用 · ${writableRootCount} 可写</span></div>
      <div><strong>后台扫描队列</strong><span>${Number(health.jobs?.scanRunning || 0)} 个运行中 · 失败 ${Number(health.jobs?.scanFailed || 0)}</span></div>
    </div>
  </section>`;

  return `<section class="section">
    <div class="admin-cards">
      <div class="admin-card"><span class="eyebrow">服务器健康</span><strong>${escapeHtml(health.status || "OK")}</strong><span>架构 v${escapeHtml(health.schemaVersion || "1")}</span></div>
      <div class="admin-card"><span class="eyebrow">媒体库</span><strong>${libraries.length}</strong><span>已挂载媒体库</span></div>
      <div class="admin-card"><span class="eyebrow">用户</span><strong>${users.length}</strong><span>活跃账号</span></div>
    </div>
  </section>${healthDetails}`;
}

function renderAdminLibraries({ libraries = [] }) {
  const libraryCards = libraries.map(renderAdminLibrary).join("");
  return `<section class="section"><div class="section-heading"><h2>新建媒体库</h2><span>创建媒体库并绑定 NAS 本地文件目录</span></div>
  <form class="admin-form" data-action="create-library">
    <input name="name" placeholder="媒体库名称 (如: 电影)" aria-label="媒体库名称" required>
    <select name="kind" aria-label="媒体库类型"><option value="MOVIE">电影库</option><option value="SERIES">电视剧库</option><option value="MIXED">混合类型库</option></select>
    <button class="button" type="submit">创建库</button>
  </form></section>
  <section class="section"><div class="section-heading"><h2>已配置媒体库</h2><span>支持增量扫描与挂载路径管理</span></div>
  <div class="admin-library-grid">${libraryCards || '<div class="empty"><h3>暂无媒体库</h3></div>'}</div></section>`;
}

function renderAdminUsers({ libraries = [], users = [], access = {} }) {
  const userRows = users.map((user) => `<tr>
    <td>${escapeHtml(user.displayName || user.usernameNormalized)}<small>${escapeHtml(user.usernameNormalized)}</small></td>
    <td>${user.isDisabled ? '<span class="badge badge-red">已禁用</span>' : user.canManageServer ? '<span class="badge badge-indigo">管理员</span>' : '<span class="badge badge-emerald">普通用户</span>'}</td>
    <td><a class="button secondary" href="#user-${escapeHtml(user.id)}">设置</a> ${user.isDisabled ? "" : `<button class="button secondary" data-disable-user="${escapeHtml(user.id)}">禁用</button>`}</td>
  </tr>`).join("");
  
  const userEditors = users.map((user) => renderUserEditor(user, libraries, new Set(access[user.id] || []))).join("");

  return `<section class="section"><div class="section-heading"><h2>用户与权限管理</h2><span>创建并管理可以登录 Lux 的账号</span></div>
  <form class="admin-form" data-action="create-user">
    <input name="username" placeholder="用户名" aria-label="用户名" autocomplete="username" required>
    <input name="displayName" placeholder="显示名称" aria-label="显示名称" autocomplete="name">
    <input name="password" type="password" placeholder="密码" aria-label="密码" autocomplete="new-password" required>
    <label class="check"><input name="isAdmin" type="checkbox"> 授予管理员权限</label>
    <button class="button" type="submit">新建用户</button>
  </form>
  <div class="table-wrap"><table><thead><tr><th>用户</th><th>角色</th><th>操作</th></tr></thead><tbody>${userRows}</tbody></table></div>
  ${userEditors}</section>`;
}

function renderUserEditor(user, libraries, accessibleIds) {
  const checkboxes = libraries.map((library) => `<label class="check"><input type="checkbox" data-user-library-access="${escapeHtml(user.id)}|${escapeHtml(library.id)}"${accessibleIds.has(library.id) ? " checked" : ""}> ${escapeHtml(library.name)}</label>`).join("");
  return `<details class="admin-user" id="user-${escapeHtml(user.id)}"><summary><strong>${escapeHtml(user.displayName || user.usernameNormalized)}</strong> (${escapeHtml(user.usernameNormalized)})</summary>
  <form class="admin-form user-edit-form" data-action="update-user" data-user-id="${escapeHtml(user.id)}">
    <input name="displayName" value="${escapeHtml(user.displayName || "")}" placeholder="显示名称">
    <input name="password" type="password" placeholder="新密码 (留空则不修改)">
    <label class="check"><input name="isDisabled" type="checkbox"${user.isDisabled ? " checked" : ""}> 禁用该账号</label>
    <label class="check"><input name="canManageServer" type="checkbox"${user.canManageServer ? " checked" : ""}> 管理员权限</label>
    <button class="button" type="submit">保存更新</button>
  </form>
  <div class="permission-grid"><span class="sidebar-label">媒体库访问权限:</span>${checkboxes || "<span>暂无媒体库</span>"}</div></details>`;
}

function renderAdminLibrary(library) {
  const roots = (library.roots || []).map((root) => `<li><div><strong>${escapeHtml(root.canonicalPath || root.displayPath)}</strong><span>${root.isAvailable ? "🟢 可访问" : "🔴 不可用"} · ${root.isWritable ? "可写" : "只读"}</span></div><button class="button secondary" type="button" data-delete-root="${escapeHtml(library.id)}|${escapeHtml(root.id)}">移除路径</button></li>`).join("");
  return `<article class="admin-library"><div class="section-heading"><div><h3>${escapeHtml(library.name)}</h3><span>${escapeHtml(library.kind)}</span></div><button class="button" type="button" data-scan-library="${escapeHtml(library.id)}">⚡ 立即扫描</button></div>
  <ul class="admin-list">${roots || '<li><span>尚未添加媒体路径</span></li>'}</ul>
  <form class="admin-form compact-form" data-action="add-library-root" data-library-id="${escapeHtml(library.id)}">
    <input name="path" placeholder="NAS 本地路径 (如 /media/movies)" required>
    <button class="button secondary" type="submit">添加路径</button>
  </form></article>`;
}

function renderAdminJobs({ jobs = [] }) {
  const jobRows = jobs.map((job) => `<tr>
    <td><strong>${escapeHtml(job.jobType || "SCAN")}</strong><small>${escapeHtml(job.id)}</small></td>
    <td><span class="badge ${job.status === "FAILED" ? "badge-red" : job.status === "COMPLETED" ? "badge-emerald" : "badge-amber"}">${escapeHtml(job.status)}</span></td>
    <td>${escapeHtml(job.createdAt || "")}</td>
    <td><button class="button secondary" type="button" data-view-job="${escapeHtml(job.id)}">查看详情</button></td>
  </tr>`).join("");

  return `<section class="section"><div class="section-heading"><h2>后台任务管理</h2><span>包含全量校验与网络刮削任务</span></div>
  <div class="table-wrap"><table><thead><tr><th>任务类型</th><th>状态</th><th>创建时间</th><th>操作</th></tr></thead><tbody>${jobRows || '<tr><td colspan="4">暂无历史任务</td></tr>'}</tbody></table></div></section>`;
}

function renderAdminMetadata({ pending = [] }) {
  const pendingRows = pending.map((item) => `<tr>
    <td><strong>${escapeHtml(item.title || item.name)}</strong><small>${escapeHtml(item.id)}</small></td>
    <td><span class="badge badge-amber">PENDING 待处理</span></td>
    <td><button class="button secondary" type="button" data-route="item" data-item="${escapeHtml(item.id)}">搜刮并确认</button></td>
  </tr>`).join("");

  return `<section class="section"><div class="section-heading"><h2>待匹配元数据队列</h2><span>搜刮置信度较低或缺乏 NFO 的条目</span></div>
  <div class="table-wrap"><table><thead><tr><th>条目标题</th><th>匹配状态</th><th>操作</th></tr></thead><tbody>${pendingRows || '<tr><td colspan="3">🎉 待处理队列为空，所有影视条目均已完成元数据匹配！</td></tr>'}</tbody></table></div></section>`;
}

function bind() {
  const forms = app.querySelectorAll("form[data-action]");
  forms.forEach((form) => {
    form.onsubmit = async (event) => {
      event.preventDefault();
      const action = form.dataset.action;
      state.error = "";
      state.notice = "";
      try {
        if (action === "login") {
          const user = await api.login(field(form, "username").value, field(form, "password").value);
          state.user = user;
          state.route = "home";
          render();
        } else if (action === "setup") {
          const result = await api.setup({
            username: field(form, "username").value,
            displayName: field(form, "displayName").value,
            password: field(form, "password").value,
            libraryName: field(form, "libraryName").value,
            libraryKind: field(form, "libraryKind").value,
            libraryRoot: field(form, "libraryRoot").value,
          });
          state.initialized = true;
          state.user = result.user;
          state.route = "home";
          render();
        } else if (action === "search") {
          const query = field(form, "q").value.trim();
          if (query) {
            state.query = query;
            state.searchPage = 1;
            state.route = "search";
            render();
          }
        } else if (action === "library-filter") {
          const formData = new FormData(form);
          state.libraryFilters = Object.fromEntries(formData.entries());
          state.libraryPage = 1;
          render();
        } else if (action === "create-library") {
          await api.createLibrary({
            name: field(form, "name").value,
            kind: field(form, "kind").value,
          });
          loadRoute();
        } else if (action === "add-library-root") {
          const libraryId = form.dataset.libraryId;
          await api.addLibraryRoot(libraryId, field(form, "path").value);
          loadRoute();
        } else if (action === "create-user") {
          await api.createUser({
            username: field(form, "username").value,
            displayName: field(form, "displayName").value,
            password: field(form, "password").value,
            isAdmin: field(form, "isAdmin").checked,
          });
          loadRoute();
        } else if (action === "update-user") {
          const userId = form.dataset.userId;
          await api.updateUser(userId, {
            displayName: field(form, "displayName").value,
            password: field(form, "password").value || undefined,
            isDisabled: field(form, "isDisabled").checked,
            canManageServer: field(form, "canManageServer").checked,
          });
          loadRoute();
        } else if (action === "update-settings") {
          const update = {};
          const playedField = field(form, "resumePlayedPercent");
          const minimumField = field(form, "resumeMinTicks");
          if (playedField) update.resumePlayedPercent = Number(playedField.value);
          if (minimumField) update.resumeMinTicks = Number(minimumField.value);
          const networkProxyField = field(form, "networkProxyUrl");
          if (networkProxyField) {
            update.networkProxyUrl = networkProxyField.value.trim() || null;
            state.networkProxyDiagnostics = null;
          }
          await api.updateSettings(update);
          state.notice = field(form, "networkProxyUrl")
            ? "网络代理设置已保存，重启 Lux 后生效"
            : "设置已成功更新";
          render();
        } else if (action === "search-candidates") {
          const query = field(form, "query").value;
          const year = field(form, "year").value ? Number(field(form, "year").value) : undefined;
          const result = await api.searchCandidates(state.itemId, query, year);
          state.itemCandidates = result.items || [];
          loadRoute();
        }
      } catch (error) {
        state.error = error.message;
        render();
      }
    };
  });

  app.onclick = async (event) => {
    const target = event.target.closest("button[data-route], button[data-library], button[data-item], button[data-action], button[data-season], button[data-delete-root], button[data-scan-library], button[data-disable-user], button[data-select-candidate], button[data-delete-image], button[data-network-proxy-test], input[data-user-library-access]");
    if (!target) return;

    if (target.dataset.networkProxyTest !== undefined) {
      target.disabled = true;
      state.error = "";
      try {
        const input = app.querySelector("input[name=networkProxyUrl]");
        state.networkProxyDiagnostics = await api.testNetworkProxy(input?.value.trim() || undefined);
        const view = document.querySelector("#view");
        if (view) view.innerHTML = renderAdminSettings(state.admin?.settings, state.networkProxyDiagnostics);
        bind();
      } catch (error) {
        state.error = error.message;
        render();
      }
    } else if (target.dataset.route) {
      state.route = target.dataset.route;
      state.drawerOpen = false;
      render();
    } else if (target.dataset.library) {
      state.libraryId = target.dataset.library;
      state.libraryPage = 1;
      state.libraryFilters = {};
      state.route = "library";
      render();
    } else if (target.dataset.item) {
      state.itemId = target.dataset.item;
      state.route = "item";
      render();
    } else if (target.dataset.season) {
      const children = await api.children(target.dataset.season);
      const view = document.querySelector("#children-panel");
      if (view) view.innerHTML = renderChildrenPanel(state.item, children, true);
    } else if (target.dataset.action === "toggle-drawer") {
      state.drawerOpen = !state.drawerOpen;
      render();
    } else if (target.dataset.action === "close-drawer") {
      state.drawerOpen = false;
      render();
    } else if (target.dataset.action === "toggle-admin-nav") {
      state.adminNavExpanded = !state.adminNavExpanded;
      render();
    } else if (target.dataset.action === "logout") {
      await api.logout();
      state.user = null;
      render();
    } else if (target.dataset.action === "toggle-favorite") {
      const current = Boolean(state.playback?.isFavorite);
      await api.favorite(state.itemId, !current);
      state.playback.isFavorite = !current;
      loadRoute();
    } else if (target.dataset.action === "toggle-played") {
      const current = Boolean(state.playback?.isPlayed);
      await api.played(state.itemId, !current);
      state.playback.isPlayed = !current;
      loadRoute();
    } else if (target.dataset.action === "clear-library-filter") {
      state.libraryFilters = {};
      state.libraryPage = 1;
      loadRoute();
    } else if (target.dataset.scanLibrary) {
      await api.scanLibrary(target.dataset.scanLibrary);
      state.notice = "已触发扫描任务";
      render();
    } else if (target.dataset.deleteRoot) {
      const [libId, rootId] = target.dataset.deleteRoot.split("|");
      await api.deleteLibraryRoot(libId, rootId);
      loadRoute();
    } else if (target.dataset.disableUser) {
      await api.disableUser(target.dataset.disableUser);
      loadRoute();
    } else if (target.dataset.selectCandidate) {
      const [itemId, candId, mode] = target.dataset.selectCandidate.split("|");
      await api.selectCandidate(itemId, candId, mode);
      state.notice = "元数据匹配候选已应用，元数据与 NFO 回写完成";
      loadRoute();
    } else if (target.dataset.deleteImage) {
      await api.deleteAdminImage(state.itemId, target.dataset.deleteImage);
      loadRoute();
    } else if (target.dataset.userLibraryAccess) {
      const [userId, libId] = target.dataset.userLibraryAccess.split("|");
      await api.setLibraryAccess(userId, libId, target.checked);
    }
  };
}

// Global Initialization
(async function init() {
  try {
    const setup = await api.setupStatus();
    state.initialized = setup.initialized;
    if (state.initialized) {
      state.user = await api.me().catch(() => null);
    }
  } catch (error) {
    state.error = error.message;
  }
  render();
})();
