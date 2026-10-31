/**
 * 更新日志页面的入口：版本条目按大版本系列放在 `changelog/releases/`，
 * 渲染拆到 `changelog/` 下的小组件，这里只保留对外的导出路径。
 */
export { AdminChangelogPage } from "./changelog/AdminChangelogPage";
export { changelogReleases } from "./changelog/releases";
export type { ChangelogRelease, ChangelogSection, ChangelogSectionKind } from "./changelog/types";
