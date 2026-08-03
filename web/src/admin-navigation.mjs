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
