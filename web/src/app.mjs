const app = document.querySelector("#app");
const state = { user: null, initialized: true, libraries: [], home: null, admin: null, route: "home", item: null, error: "", setupNotice: "" };

const api = {
  async request(path, options = {}) {
    const headers = { Accept: "application/json" };
    if (options.body) headers["Content-Type"] = "application/json";
    const response = await fetch(path, { credentials: "same-origin", headers, ...options });
    if (response.status === 204) return null;
    const body = await response.json().catch(() => ({}));
    if (!response.ok) throw new Error(body?.error?.message || "请求失败");
    return body;
  },
  login(username, password) { return this.request("/api/v1/auth/login", { method: "POST", body: JSON.stringify({ username, password }) }); },
  setup(username, displayName, password) { return this.request("/api/v1/setup/complete", { method: "POST", body: JSON.stringify({ username, displayName, password }) }); },
  logout() { return this.request("/api/v1/auth/logout", { method: "POST", headers: { "x-csrf-token": readCookie("lux_csrf") } }); },
  setupStatus() { return this.request("/api/v1/setup/status"); },
  me() { return this.request("/api/v1/auth/me"); },
  home() { return this.request("/api/v1/home"); },
  libraries() { return this.request("/api/v1/libraries"); },
  libraryItems(id) { return this.request("/api/v1/libraries/" + encodeURIComponent(id) + "/items?page=1&pageSize=60"); },
  item(id) { return this.request("/api/v1/items/" + encodeURIComponent(id)); },
  search(query) { return this.request("/api/v1/search?q=" + encodeURIComponent(query) + "&page=1&pageSize=60"); },
  adminUsers() { return this.request("/api/v1/admin/users"); },
  createUser(data) { return this.request("/api/v1/admin/users", { method: "POST", headers: { "x-csrf-token": readCookie("lux_csrf") }, body: JSON.stringify(data) }); },
  disableUser(id) { return this.request("/api/v1/admin/users/" + encodeURIComponent(id), { method: "DELETE", headers: { "x-csrf-token": readCookie("lux_csrf") } }); },
  adminLibraries() { return this.request("/api/v1/admin/libraries"); },
  audit() { return this.request("/api/v1/admin/audit?page=1&pageSize=50"); },
  ready() { return fetch("/health/ready", { credentials: "same-origin" }).then((response) => response.json()); },
};

function readCookie(name) {
  const found = document.cookie.split("; ").find((part) => part.startsWith(name + "="));
  return found ? found.slice(name.length + 1) : "";
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
  if (state.route === "search") return "搜索";
  if (state.route === "item") return state.item?.title || "详情";
  if (state.route === "admin") return "管理控制台";
  return "你的片单";
}

function render() {
  if (state.initialized === false) return renderSetup();
  if (!state.user) return renderAuth();
  const error = state.error ? "<div class=\"notice error\" role=\"alert\">" + escapeHtml(state.error) + "</div>" : "";
  app.innerHTML = "<div class=\"shell\"><aside class=\"sidebar\">" + brand() + nav() + account() + "</aside><div><nav class=\"mobile-nav\"><strong>Lux</strong><button class=\"button secondary\" data-action=\"logout\">退出</button></nav><main class=\"content\"><header class=\"topbar\"><div><span class=\"eyebrow\">Personal media</span><h1>" + titleForRoute() + "</h1></div><form class=\"search-form\" data-action=\"search\"><input class=\"search-box\" name=\"q\" type=\"search\" placeholder=\"搜索电影、剧集或别名\" aria-label=\"搜索\"></form></header>" + error + "<section id=\"view\">" + loading() + "</section></main></div></div>";
  bind();
  loadRoute();
}

function brand() { return "<div class=\"brand\"><strong>Lux</strong><span>quietly yours</span></div>"; }
function nav() {
  const homeCurrent = state.route === "home" ? "page" : "false";
  const libraryCurrent = state.route === "libraries" ? "page" : "false";
  const admin = state.user.canManageServer ? "<button data-route=\"admin\" aria-current=\"" + (state.route === "admin" ? "page" : "false") + "\">管理</button>" : "";
  return "<nav class=\"nav\" aria-label=\"主导航\"><button data-route=\"home\" aria-current=\"" + homeCurrent + "\">首页</button><button data-route=\"libraries\" aria-current=\"" + libraryCurrent + "\">媒体库</button>" + admin + "</nav>";
}
function account() {
  return "<div class=\"sidebar-footer\"><small>" + escapeHtml(state.user.displayName || state.user.usernameNormalized) + "</small><button class=\"button secondary\" data-action=\"logout\" style=\"margin-top:.7rem;width:100%\">退出登录</button></div>";
}

function renderAuth() {
  const error = state.error ? "<div class=\"notice error\" role=\"alert\">" + escapeHtml(state.error) + "</div>" : "";
  const notice = state.setupNotice ? "<div class=\"notice\" role=\"status\">" + escapeHtml(state.setupNotice) + "</div>" : "";
  app.innerHTML = "<div class=\"auth-layout\"><section class=\"auth-card\"><span class=\"eyebrow\">Personal media</span><h1 style=\"margin-top:.7rem\">Lux</h1><p>把你的媒体库安静地放在自己的设备上。</p>" + notice + error + "<form data-action=\"login\"><div class=\"field\"><label for=\"username\">用户名</label><input id=\"username\" name=\"username\" autocomplete=\"username\" required></div><div class=\"field\"><label for=\"password\">密码</label><input id=\"password\" name=\"password\" type=\"password\" autocomplete=\"current-password\" required></div><div class=\"form-actions\"><button class=\"button\" type=\"submit\">登录</button></div></form></section></div>";
  bind();
}

function renderSetup() {
  const error = state.error ? "<div class=\"notice error\" role=\"alert\">" + escapeHtml(state.error) + "</div>" : "";
  app.innerHTML = "<div class=\"auth-layout\"><section class=\"auth-card\"><span class=\"eyebrow\">First run</span><h1 style=\"margin-top:.7rem\">开始使用 Lux</h1><p>创建第一个服务器管理员。媒体库和 TMDb 设置可以稍后在控制台完成。</p>" + error + "<form data-action=\"setup\"><div class=\"field\"><label for=\"setup-username\">管理员用户名</label><input id=\"setup-username\" name=\"username\" autocomplete=\"username\" required></div><div class=\"field\"><label for=\"setup-display-name\">显示名称</label><input id=\"setup-display-name\" name=\"displayName\" autocomplete=\"name\"></div><div class=\"field\"><label for=\"setup-password\">管理员密码</label><input id=\"setup-password\" name=\"password\" type=\"password\" autocomplete=\"new-password\" minlength=\"8\" required></div><div class=\"form-actions\"><button class=\"button\" type=\"submit\">完成初始化</button></div></form></section></div>";
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
    } else if (state.route === "search") {
      const result = await api.search(state.query);
      view.innerHTML = renderGrid(result.items || [], "搜索“" + escapeHtml(state.query) + "”");
    } else if (state.route === "item") {
      state.item = await api.item(state.itemId);
      view.innerHTML = renderDetail(state.item);
    } else if (state.route === "admin") {
      state.admin = await Promise.all([api.ready(), api.adminLibraries(), api.adminUsers(), api.audit()]);
      view.innerHTML = renderAdmin();
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
function renderGrid(items, heading = "") {
  const title = heading ? "<div class=\"section-heading\" style=\"grid-column:1/-1\"><h2>" + heading + "</h2><span>" + items.length + " 项</span></div>" : "";
  const content = items.length ? items.map(mediaCard).join("") : "<div class=\"empty\" style=\"grid-column:1/-1\"><h3>没有找到内容</h3><p>试试其他关键词或筛选条件。</p></div>";
  return "<div class=\"media-grid\">" + title + content + "</div>";
}
function mediaCard(item) {
  return "<button class=\"media-card\" data-item=\"" + escapeHtml(item.id) + "\">" + poster(item) + "<span class=\"media-card-body\"><strong>" + escapeHtml(item.title || item.name) + "</strong><span class=\"media-meta\">" + escapeHtml(item.productionYear || item.itemType || "") + "</span></span></button>";
}
function renderDetail(item) {
  const sources = item.mediaSources || [];
  const chips = sources.map((source) => "<span class=\"chip\">" + escapeHtml(source.qualityLabel || source.editionName || source.container || "source") + "</span>").join("");
  const buttons = sources.map((source, index) => "<button class=\"button secondary\" data-source=\"" + escapeHtml(source.id) + "\" aria-pressed=\"" + (index === 0) + "\">" + escapeHtml(source.qualityLabel || source.editionName || source.container || "版本 " + (index + 1)) + "</button>").join("");
  const player = sources.length ? "<div class=\"source-list\" aria-label=\"媒体版本\">" + buttons + "</div><video class=\"player\" controls preload=\"metadata\" data-player src=\"/api/v1/items/" + encodeURIComponent(item.id) + "/stream?sourceId=" + encodeURIComponent(sources[0].id) + "\"></video>" : "";
  return "<a class=\"back-link\" href=\"#home\" data-route=\"home\">← 返回</a><article class=\"detail\"><div>" + poster(item, "detail-poster") + "</div><div class=\"detail-copy\"><span class=\"eyebrow\">" + escapeHtml(item.itemType || item.type || "media") + "</span><h2 style=\"margin-top:.6rem\">" + escapeHtml(item.title || item.name) + "</h2><div class=\"chips\">" + (item.productionYear ? "<span class=\"chip\">" + item.productionYear + "</span>" : "") + chips + "</div><p>" + escapeHtml(item.overview || "暂无简介。") + "</p>" + player + "</div></article>";
}

function renderAdmin() {
  const ready = state.admin[0] || {};
  const libraries = state.admin[1]?.libraries || [];
  const users = state.admin[2]?.users || [];
  const events = state.admin[3]?.events || [];
  const userRows = users.map((user) => "<tr><td>" + escapeHtml(user.displayName) + "<small>" + escapeHtml(user.usernameNormalized) + "</small></td><td>" + (user.isDisabled ? "已禁用" : user.canManageServer ? "管理员" : "普通用户") + "</td><td>" + (user.isDisabled ? "" : "<button class=\"button secondary\" data-disable-user=\"" + escapeHtml(user.id) + "\">禁用</button>") + "</td></tr>").join("");
  const libraryRows = libraries.map((library) => "<li><strong>" + escapeHtml(library.name) + "</strong><span>" + escapeHtml(library.kind) + " · " + (library.isEnabled ? "启用" : "停用") + "</span></li>").join("");
  const auditRows = events.slice(0, 8).map((event) => "<li><strong>" + escapeHtml(event.eventType) + "</strong><span>" + escapeHtml(event.actorUsername || "system") + " · " + escapeHtml(event.targetId || "") + "</span></li>").join("");
  return "<section class=\"section\"><div class=\"admin-cards\"><div class=\"admin-card\"><span class=\"eyebrow\">Health</span><strong>" + escapeHtml(ready.status || "unknown") + "</strong><span>schema " + escapeHtml(ready.schemaVersion || "—") + "</span></div><div class=\"admin-card\"><span class=\"eyebrow\">Libraries</span><strong>" + libraries.length + "</strong><span>已配置媒体库</span></div><div class=\"admin-card\"><span class=\"eyebrow\">Users</span><strong>" + users.length + "</strong><span>账户</span></div></div></section><section class=\"section\"><div class=\"section-heading\"><h2>用户与权限</h2><span>禁用不会删除历史状态</span></div><form class=\"admin-form\" data-action=\"create-user\"><input name=\"username\" placeholder=\"用户名\" aria-label=\"用户名\" required><input name=\"password\" type=\"password\" placeholder=\"临时密码\" aria-label=\"临时密码\" required><button class=\"button\" type=\"submit\">创建用户</button></form><div class=\"table-wrap\"><table><thead><tr><th>用户</th><th>角色</th><th>操作</th></tr></thead><tbody>" + userRows + "</tbody></table></div></section><section class=\"section admin-columns\"><div><div class=\"section-heading\"><h2>媒体库</h2></div><ul class=\"admin-list\">" + libraryRows + "</ul></div><div><div class=\"section-heading\"><h2>最近审计</h2></div><ul class=\"admin-list\">" + auditRows + "</ul></div></section>";
}

function bind() {
  document.querySelectorAll("[data-route]").forEach((element) => element.addEventListener("click", (event) => { event.preventDefault(); state.route = element.dataset.route; state.error = ""; render(); }));
  document.querySelectorAll("[data-library]").forEach((element) => element.addEventListener("click", async () => {
    try { const result = await api.libraryItems(element.dataset.library); document.querySelector("#view").innerHTML = renderGrid(result.items || [], "媒体库内容"); bind(); }
    catch (error) { state.error = error.message; render(); }
  }));
  document.querySelectorAll("[data-item]").forEach((element) => element.addEventListener("click", () => { state.itemId = element.dataset.item; state.route = "item"; state.error = ""; render(); }));
  document.querySelectorAll("[data-source]").forEach((element) => element.addEventListener("click", () => {
    const player = document.querySelector("[data-player]");
    if (!player) return;
    player.src = "/api/v1/items/" + encodeURIComponent(state.item.id) + "/stream?sourceId=" + encodeURIComponent(element.dataset.source);
    document.querySelectorAll("[data-source]").forEach((button) => button.setAttribute("aria-pressed", String(button === element)));
    player.play().catch(() => {});
  }));
  document.querySelectorAll("[data-disable-user]").forEach((element) => element.addEventListener("click", async () => {
    if (!window.confirm("确认禁用这个用户？")) return;
    try { await api.disableUser(element.dataset.disableUser); state.error = ""; render(); } catch (error) { state.error = error.message; render(); }
  }));
  document.querySelectorAll("[data-action='logout']").forEach((element) => element.addEventListener("click", async () => { try { await api.logout(); } finally { state.user = null; render(); } }));
  document.querySelectorAll("[data-action='retry']").forEach((element) => element.addEventListener("click", () => { state.error = ""; render(); }));
  document.querySelectorAll("form[data-action='login']").forEach((form) => form.addEventListener("submit", async (event) => {
    event.preventDefault();
    const button = form.querySelector("button"); button.disabled = true;
    try { const body = await api.login(form.username.value, form.password.value); state.user = body.user; state.error = ""; render(); }
    catch (error) { state.error = error.message; render(); }
  }));
  document.querySelectorAll("form[data-action='setup']").forEach((form) => form.addEventListener("submit", async (event) => {
    event.preventDefault();
    const button = form.querySelector("button"); button.disabled = true;
    try {
      await api.setup(form.username.value, form.displayName.value, form.password.value);
      state.initialized = true; state.error = ""; state.setupNotice = "初始化完成，请使用刚创建的管理员登录。"; render();
    } catch (error) { state.error = error.message; render(); }
  }));
  document.querySelectorAll("form[data-action='create-user']").forEach((form) => form.addEventListener("submit", async (event) => {
    event.preventDefault();
    try { await api.createUser({ username: form.username.value, password: form.password.value }); state.route = "admin"; state.error = ""; render(); }
    catch (error) { state.error = error.message; render(); }
  }));
  document.querySelectorAll("form[data-action='search']").forEach((form) => form.addEventListener("submit", (event) => {
    event.preventDefault(); const query = form.q.value.trim(); if (!query) return;
    state.query = query; state.route = "search"; state.error = ""; render();
  }));
}

async function boot() {
  try {
    state.initialized = (await api.setupStatus()).initialized;
    if (state.initialized) state.user = (await api.me()).user;
  } catch { state.user = null; }
  render();
}
boot();
