// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { AdminChangelogPage, changelogReleases } from "../src/features/admin/AdminChangelogPage";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const sectionLabels = { added: "新增", changed: "变更", fixed: "修复" } as const;

describe("AdminChangelogPage timeline structure", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    act(() => {
      root.render(
        <MemoryRouter>
          <AdminChangelogPage />
        </MemoryRouter>,
      );
    });
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
  });

  it("renders one timeline release per changelog entry, newest first", () => {
    const releases = [...container.querySelectorAll<HTMLElement>(".lux-changelog-timeline .lux-changelog-release")];
    expect(releases).toHaveLength(changelogReleases.length);
    expect(releases.map((release) => release.querySelector("h2")?.textContent))
      .toEqual(changelogReleases.map((release) => `v${release.version}`));
  });

  it("keeps each release header, date and rail markup intact", () => {
    const releaseElements = [...container.querySelectorAll<HTMLElement>(".lux-changelog-timeline .lux-changelog-release")];
    changelogReleases.forEach((release, index) => {
      const element = releaseElements[index];
      expect(element.querySelector(".lux-changelog-release-rail")?.getAttribute("aria-hidden")).toBe("true");
      expect(element.querySelector(".lux-changelog-release-body > .lux-changelog-release-header")).toBeTruthy();
      expect(element.querySelector(".lux-changelog-release-kicker")?.textContent).toBe("RELEASE");
      const time = element.querySelector("header time");
      expect(time?.getAttribute("datetime")).toBe(release.date);
      expect(time?.textContent).toBe(release.date);
    });
  });

  it("renders every section with its kind class, label and items", () => {
    const releaseElements = [...container.querySelectorAll<HTMLElement>(".lux-changelog-timeline .lux-changelog-release")];
    changelogReleases.forEach((release, index) => {
      const sections = [...releaseElements[index].querySelectorAll<HTMLElement>(".lux-changelog-sections > section")];
      expect(sections.map((section) => section.className))
        .toEqual(release.sections.map((section) => `lux-changelog-section is-${section.kind}`));
      expect(sections.map((section) => section.querySelector("h3")?.textContent))
        .toEqual(release.sections.map((section) => sectionLabels[section.kind]));
      expect(sections.map((section) => section.querySelector("h3 svg")).map(Boolean))
        .toEqual(release.sections.map(() => true));
      sections.forEach((section, sectionIndex) => {
        const items = [...section.querySelectorAll("li")];
        expect(items.map((item) => item.textContent)).toEqual(release.sections[sectionIndex].items);
      });
    });
  });

  it("summarizes the latest release in the header and the release count in the intro", () => {
    const latest = changelogReleases[0];
    const current = container.querySelector(".lux-changelog-current");
    expect(current?.getAttribute("aria-label")).toBe(`当前版本 ${latest.version}`);
    expect(current?.querySelector("strong")?.textContent).toBe(`v${latest.version}`);
    expect(current?.querySelector("small")?.textContent).toBe(latest.date);

    expect(container.querySelector(".lux-changelog-header h1")?.textContent).toBe("更新日志");
    expect(container.querySelector(".lux-changelog-header .lux-eyebrow")?.textContent).toBe("项目历史");
    expect(container.querySelector(".lux-changelog-intro > span")?.textContent).toBe(`${changelogReleases.length} 个版本`);
    expect(container.querySelector(".lux-changelog-note")?.textContent)
      .toBe("日志根据仓库源码版本记录整理，日期不等同于各渠道的正式发布日期。");
    expect(container.querySelectorAll("main.lux-admin-page.lux-changelog-page")).toHaveLength(1);
    expect(container.querySelectorAll(".lux-changelog-section")).toHaveLength(
      changelogReleases.reduce((total, release) => total + release.sections.length, 0),
    );
  });
});

describe("changelogReleases data", () => {
  it("lists releases in strictly descending semantic version order", () => {
    const parts = changelogReleases.map((release) => release.version.split(".").map(Number));
    parts.forEach((part) => expect(part).toHaveLength(3));
    for (let index = 1; index < parts.length; index += 1) {
      const previous = parts[index - 1];
      const current = parts[index];
      const isDescending = previous[0] > current[0]
        || (previous[0] === current[0] && previous[1] > current[1])
        || (previous[0] === current[0] && previous[1] === current[1] && previous[2] > current[2]);
      expect(isDescending, `${previous.join(".")} should sort before ${current.join(".")}`).toBe(true);
    }
    expect(new Set(changelogReleases.map((release) => release.version)).size).toBe(changelogReleases.length);
  });

  it("keeps every release dated and every section populated", () => {
    changelogReleases.forEach((release) => {
      expect(release.date).toMatch(/^\d{4}-\d{2}-\d{2}$/);
      expect(release.sections.length).toBeGreaterThan(0);
      release.sections.forEach((section) => {
        expect(Object.keys(sectionLabels)).toContain(section.kind);
        expect(section.items.length).toBeGreaterThan(0);
        section.items.forEach((item) => expect(item.trim()).not.toBe(""));
      });
    });
  });
});
