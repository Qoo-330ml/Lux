import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");

function lightThemeRule(selector: string) {
  const escapedSelector = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const rules = [...stylesheet.matchAll(
    new RegExp(`html\\[data-lux-theme="light"\\] ${escapedSelector}\\s*\\{([^}]*)\\}`, "g"),
  )];
  return rules.at(-1)?.[1] ?? "";
}

describe("light theme contrast", () => {
  it("keeps home content text readable on the light background", () => {
    expect(lightThemeRule(".lux-home-content")).toContain("background: var(--lux-bg)");
    expect(lightThemeRule(".lux-section-heading h2")).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-library-card strong")).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-media-copy strong")).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-empty-card")).toContain("color: var(--lux-muted)");
  });

  it("uses a light hero mask and readable dark hero copy", () => {
    expect(lightThemeRule(".lux-hero-overlay")).toContain("rgba(244,243,241");
    expect(lightThemeRule(".lux-hero-overlay")).not.toContain("rgba(0,0,0");
    expect(lightThemeRule(".lux-hero h1")).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-hero-copy p")).toContain("color: var(--lux-muted)");
    expect(lightThemeRule(".lux-app.is-home-route .lux-header")).toContain("rgba(244,243,241");
  });

  it("uses light surfaces and dark text throughout the admin dashboard", () => {
    expect(lightThemeRule(".lux-admin-sidebar")).toContain("background:");
    expect(lightThemeRule(".lux-admin-page-heading h1")).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-admin-stat")).toContain("background:");
    expect(lightThemeRule(".lux-admin-stat strong")).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-admin-panel")).toContain("background:");
    expect(lightThemeRule(".lux-admin-panel-heading h2")).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-admin-check")).toContain("color: var(--lux-text)");
  });

  it("keeps auxiliary admin states readable in light mode", () => {
    expect(lightThemeRule(".lux-admin-empty h2")).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-admin-plugin-content h2")).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-admin-plugin-icon")).toContain("color: var(--lux-text)");
  });

  it("keeps the mobile navigation readable in light mode", () => {
    expect(lightThemeRule(".lux-mobile-nav")).toContain("background: rgba(255,255,255");
    expect(lightThemeRule(".lux-mobile-nav")).toContain("box-shadow: 0 18px 44px rgba(30,30,38");
    expect(lightThemeRule(".lux-mobile-nav .lux-nav-link")).toContain("color: var(--lux-muted)");
    expect(lightThemeRule(".lux-mobile-nav .lux-nav-link:hover")).toContain("background: rgba(28,28,34");
  });

  it("keeps light-theme header controls on a light surface", () => {
    expect(lightThemeRule(".lux-icon-button")).toContain("background: rgba(255,255,255");
    expect(lightThemeRule(".lux-icon-button:hover")).toContain("background: rgba(28,28,34");
  });

  it("keeps the plugin configuration dialog readable in light mode", () => {
    expect(lightThemeRule(".lux-admin-plugin-dialog")).toContain("background: rgba(255,255,255");
    expect(lightThemeRule(".lux-admin-plugin-dialog-heading h2")).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-admin-plugin-dialog-form input")).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-admin-plugin-dialog-form input")).toContain("background: rgba(28,28,34");
  });

  it("keeps the media detail page light and readable", () => {
    expect(lightThemeRule(".lux-detail-page")).toContain("background: var(--lux-bg)");
    expect(lightThemeRule(".lux-detail-overlay")).toContain("rgba(244,243,241");
    expect(lightThemeRule(".lux-detail-overlay")).not.toContain("rgba(5,5,6");
    expect(lightThemeRule(".lux-detail-copy h1")).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-detail-overview")).toContain("color: var(--lux-muted)");
  });

  it("keeps detail versions and media cards readable in light mode", () => {
    expect(lightThemeRule(".lux-source-option")).toContain("background: rgba(255,255,255");
    expect(lightThemeRule(".lux-source-option.is-selected")).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-media-info-row")).toContain("background: rgba(255,255,255");
    expect(lightThemeRule(".lux-media-info-row > strong")).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-media-stream-card")).toContain("background: rgba(255,255,255");
    expect(lightThemeRule(".lux-media-stream-heading h3")).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-season-tab.is-active")).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-episode-link")).toContain("background: rgba(255,255,255");
  });
});
