export const ADMIN_NAV_ITEMS = Object.freeze([
  { id: "dashboard", label: "仪表盘", glyph: "▦" },
  { id: "libraries", label: "媒体库与计划", glyph: "▣" },
  { id: "users", label: "用户与权限", glyph: "◉" },
  { id: "jobs", label: "任务与日志", glyph: "↻" },
  { id: "metadata", label: "元数据与图片", glyph: "✦" },
  { id: "settings", label: "服务端设置", glyph: "⚙" },
]);

export function adminRoute(section) {
  return section === "dashboard" ? "admin" : `admin-${section}`;
}

export function adminSectionForRoute(route) {
  if (route === "admin") return "dashboard";
  const section = ADMIN_NAV_ITEMS.find((item) => adminRoute(item.id) === route);
  return section?.id || "dashboard";
}

export function isAdminRoute(route) {
  return route === "admin" || ADMIN_NAV_ITEMS.some((item) => adminRoute(item.id) === route);
}

export function renderAdminNavigation({ expanded = false, route = "home" } = {}) {
  const isExpanded = Boolean(expanded && isAdminRoute(route));
  const currentSection = adminSectionForRoute(route);
  const submenu = isExpanded
    ? `<div class="nav-submenu" id="admin-navigation" role="group" aria-label="管理设置">${ADMIN_NAV_ITEMS.map((item) => `<button class="nav-subitem" data-route="${adminRoute(item.id)}" aria-current="${currentSection === item.id ? "page" : "false"}"><span class="nav-glyph">${item.glyph}</span><span>${item.label}</span></button>`).join("")}</div>`
    : "";
  return `<div class="nav-admin-group"><button class="nav-admin-toggle" data-action="toggle-admin-nav" aria-expanded="${isExpanded}" aria-controls="admin-navigation" aria-current="${isAdminRoute(route) ? "page" : "false"}"><span class="nav-glyph">⚙</span><span>管理</span><span class="nav-chevron" aria-hidden="true">${isExpanded ? "⌃" : "⌄"}</span></button>${submenu}</div>`;
}
