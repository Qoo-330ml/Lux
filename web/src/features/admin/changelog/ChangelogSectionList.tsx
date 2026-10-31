import { ArrowRightLeft, Sparkles, Wrench } from "lucide-react";
import type { ChangelogSection, ChangelogSectionKind } from "./types";

const sectionMeta: Record<ChangelogSectionKind, { label: string; icon: typeof Sparkles }> = {
  added: { label: "新增", icon: Sparkles },
  fixed: { label: "修复", icon: Wrench },
  changed: { label: "变更", icon: ArrowRightLeft },
};

/** 单个版本的分类条目列表：新增 / 修复 / 变更。 */
export function ChangelogSectionList({ sections }: { sections: ChangelogSection[] }) {
  return (
    <div className="lux-changelog-sections">
      {sections.map((section) => {
        const { icon: Icon, label } = sectionMeta[section.kind];
        return (
          <section className={`lux-changelog-section is-${section.kind}`} key={section.kind}>
            <h3><Icon size={14} aria-hidden="true" />{label}</h3>
            <ul>
              {section.items.map((item) => <li key={item}>{item}</li>)}
            </ul>
          </section>
        );
      })}
    </div>
  );
}
