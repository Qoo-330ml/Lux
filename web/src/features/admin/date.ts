export function formatAdminDate(value?: string | number | null) {
  if (value == null || value === "") return "未知时间";
  const numeric = typeof value === "number" || /^\d+$/.test(value);
  const date = new Date(numeric ? Number(value) * 1000 : value);
  return Number.isNaN(date.valueOf()) ? String(value) : date.toLocaleString("zh-CN", { dateStyle: "short", timeStyle: "short" });
}
