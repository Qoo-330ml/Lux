export function formatRelativeTime(timestamp?: number | null): string {
  if (!timestamp) return "暂无";
  const seconds = Math.floor(Date.now() / 1000 - timestamp);
  if (seconds < 60) return "刚刚";
  if (seconds < 3600) return `${Math.floor(seconds / 60)} 分钟前`;
  if (seconds < 86_400) return `${Math.floor(seconds / 3600)} 小时前`;
  if (seconds < 172_800) return "昨天";
  if (seconds < 2_592_000) return `${Math.floor(seconds / 86_400)} 天前`;
  return `${Math.floor(seconds / 2_592_000)} 个月前`;
}

export function formatTimestamp(timestamp?: number | null): string {
  return timestamp ? new Date(timestamp * 1000).toLocaleString() : "暂无";
}
