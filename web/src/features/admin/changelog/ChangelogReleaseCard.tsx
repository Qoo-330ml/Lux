import type { ChangelogRelease } from "./types";
import { ChangelogSectionList } from "./ChangelogSectionList";

/** 时间轴上的一个版本卡片：版本号、发布日期与分类条目。 */
export function ChangelogReleaseCard({ release }: { release: ChangelogRelease }) {
  return (
    <article className="lux-changelog-release">
      <div className="lux-changelog-release-rail" aria-hidden="true"><span /></div>
      <div className="lux-changelog-release-body">
        <header className="lux-changelog-release-header">
          <div>
            <span className="lux-changelog-release-kicker">RELEASE</span>
            <h2>v{release.version}</h2>
          </div>
          <time dateTime={release.date}>{release.date}</time>
        </header>
        <ChangelogSectionList sections={release.sections} />
      </div>
    </article>
  );
}
