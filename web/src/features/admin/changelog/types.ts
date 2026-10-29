/** 更新日志条目的分类，与 `.lux-changelog-section.is-<kind>` 样式一一对应。 */
export type ChangelogSectionKind = "added" | "fixed" | "changed";

export type ChangelogSection = {
  kind: ChangelogSectionKind;
  items: string[];
};

export type ChangelogRelease = {
  version: string;
  date: string;
  sections: ChangelogSection[];
};
