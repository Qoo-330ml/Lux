import { BookOpen, GitBranch } from "lucide-react";
import { changelogReleases } from "./releases";
import { ChangelogReleaseCard } from "./ChangelogReleaseCard";

export function AdminChangelogPage() {
  const latestRelease = changelogReleases[0];

  return (
    <main className="lux-admin-page lux-changelog-page">
      <header className="lux-changelog-header">
        <div className="lux-changelog-header-mark" aria-hidden="true"><BookOpen size={21} /></div>
        <div>
          <span className="lux-eyebrow">项目历史</span>
          <h1>更新日志</h1>
          <p>记录 Lux 每一次让媒体管理更清晰、更可靠的更新。</p>
        </div>
        <div className="lux-changelog-current" aria-label={`当前版本 ${latestRelease.version}`}>
          <span>当前版本</span>
          <strong>v{latestRelease.version}</strong>
          <small>{latestRelease.date}</small>
        </div>
      </header>

      <div className="lux-changelog-intro">
        <div className="lux-changelog-intro-line" aria-hidden="true"><GitBranch size={16} /></div>
        <p>从 <strong>0.1.0</strong> 开始，按版本倒序整理项目主要的用户可见变化。</p>
        <span>{changelogReleases.length} 个版本</span>
      </div>

      <div className="lux-changelog-timeline">
        {changelogReleases.map((release) => (
          <ChangelogReleaseCard key={release.version} release={release} />
        ))}
      </div>

      <p className="lux-changelog-note">日志根据仓库源码版本记录整理，日期不等同于各渠道的正式发布日期。</p>
    </main>
  );
}
