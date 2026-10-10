import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");

function lightThemeRule(selector: string) {
  const escapedSelector = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const rules = [...stylesheet.matchAll(
    new RegExp(`html\\[data-lux-theme="light"\\] ${escapedSelector}(?:\\s*,[^{}]+)?\\s*\\{([^}]*)\\}`, "g"),
  )];
  return rules.at(-1)?.[1] ?? "";
}

function lightThemeRootRule() {
  const rules = [...stylesheet.matchAll(/html\[data-lux-theme="light"\]\s*\{([^}]*)\}/g)];
  return rules.at(-1)?.[1] ?? "";
}

function cssRule(selector: string) {
  const escapedSelector = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return stylesheet.match(new RegExp(`(?:^|\\n)${escapedSelector}\\s*\\{([^}]*)\\}`))?.[1] ?? "";
}

function contrastRatio(foreground: string, background: string) {
  const luminance = (color: string) => {
    const channels = color.match(/[\da-f]{2}/gi)?.map((channel) => parseInt(channel, 16) / 255) ?? [];
    const [red = 0, green = 0, blue = 0] = channels.map((channel) =>
      channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4,
    );
    return 0.2126 * red + 0.7152 * green + 0.0722 * blue;
  };
  const values = [luminance(foreground), luminance(background)].sort((left, right) => right - left);
  return (values[0] + 0.05) / (values[1] + 0.05);
}

function lightStatusColor(name: string) {
  return lightThemeRootRule().match(new RegExp(`${name}:\\s*(#[\\da-f]{6})`, "i"))?.[1] ?? "";
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
    expect(lightThemeRule(".lux-hero-title")).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-hero-copy p")).toContain("color: var(--lux-muted)");
    expect(lightThemeRule(".lux-app.is-home-route .lux-header::before")).toContain("rgba(244,243,241");
    expect(lightThemeRule(".lux-header::before")).toContain("rgba(244,243,241");
  });

  it("uses light surfaces and dark text throughout the admin dashboard", () => {
    expect(lightThemeRule(".lux-admin-sidebar")).toContain("background:");
    expect(lightThemeRule(".lux-admin-page-heading h1")).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-admin-panel")).toContain("background:");
    expect(lightThemeRule(".lux-admin-panel-heading h2")).toContain("color: var(--lux-text)");
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

  it("keeps portaled media action menus readable in light mode", () => {
    expect(lightThemeRule(".lux-media-action-menu")).toContain("background: rgba(255,255,255");
    expect(lightThemeRule(".lux-media-action-menu")).toContain("border-color: var(--lux-line)");
    expect(lightThemeRule(".lux-media-action-menu-heading strong")).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-media-action-menu-heading > span")).toContain("background: rgba(28,28,34");
    expect(lightThemeRule(".lux-media-action svg")).toContain("color: var(--lux-muted)");
    expect(lightThemeRule(".lux-media-action:hover")).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-media-action:focus-visible")).toContain("background: rgba(28,28,34");
  });

  it("uses a pale mobile detail header mask with dark controls", () => {
    const mask = lightThemeRule(".lux-detail-mobile-header::before");
    expect(mask).toContain("rgba(244,243,241");
    expect(mask).not.toContain("rgba(5,5,6");
    expect(lightThemeRule(".lux-detail-mobile-brand")).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-detail-mobile-back")).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-detail-mobile-back")).toContain("background: rgba(255,255,255");
    expect(lightThemeRule(".lux-detail-mobile-header .lux-media-actions-trigger")).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-detail-mobile-header .lux-media-actions-trigger")).toContain("background: rgba(255,255,255");
  });

  it("uses light surfaces and readable controls in media editor dialogs", () => {
    expect(lightThemeRule(".lux-media-editor")).toContain("background: var(--lux-surface-strong)");
    expect(lightThemeRule(".lux-media-editor-header h2")).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-media-editor-close:hover")).toContain("color: var(--lux-text)");
    expect(lightThemeRule('.lux-metadata-field input:not([type="checkbox"]):not([type="radio"])')).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-image-type-tabs button:hover")).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-image-result:hover")).toContain("color: var(--lux-text)");
  });

  it("keeps mobile detail actions readable on light surfaces", () => {
    expect(lightThemeRule(".lux-detail-copy > .lux-hero-actions > .lux-detail-action-control")).toContain("background: transparent");
    expect(lightThemeRule(".lux-detail-copy > .lux-hero-actions > .lux-detail-action-control:hover")).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-detail-action-icon")).toContain("background: rgba(28,28,34,.04)");
    expect(lightThemeRule(".lux-detail-copy > .lux-hero-actions > .lux-detail-watched-status.is-played")).toContain("color: var(--lux-status-success)");
    expect(lightThemeRule(".lux-detail-inline-menu .lux-media-actions-trigger:hover")).toContain("color: var(--lux-text)");
  });

  it("uses dark text for failed page state headings", () => {
    expect(lightThemeRule(".lux-state-screen h1")).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-page-state h1")).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-admin-page-state h1")).toContain("color: var(--lux-text)");
  });

  it("uses contrast-safe colors for feedback and task statuses", () => {
    for (const token of ["--lux-status-danger", "--lux-status-success", "--lux-status-warning", "--lux-status-active"]) {
      const color = lightStatusColor(token);
      expect(color, `${token} should be defined in the light theme`).toMatch(/^#[\da-f]{6}$/i);
      expect(contrastRatio(color, "#f4f3f1"), `${token} should meet 4.5:1 on the light surface`).toBeGreaterThanOrEqual(4.5);
    }
    expect(lightThemeRule(".lux-error-copy")).toContain("color: var(--lux-status-danger)");
    expect(lightThemeRule(".lux-success-copy")).toContain("color: var(--lux-status-success)");
    expect(lightThemeRule(".lux-account-notice")).toContain("color: var(--lux-status-warning)");
    expect(lightThemeRule(".lux-operations-log-level")).toContain("color: var(--lux-status-success)");
    expect(lightThemeRule(".lux-operations-log-row.is-warn .lux-operations-log-level")).toContain("color: var(--lux-status-warning)");
    expect(lightThemeRule(".lux-operations-log-row.is-error .lux-operations-log-level")).toContain("color: var(--lux-status-danger)");
    expect(lightThemeRule(".status-running")).toContain("color: var(--lux-status-active)");
    expect(lightThemeRule(".status-completed")).toContain("color: var(--lux-status-success)");
    expect(lightThemeRule(".status-completed-with-issues")).toContain("color: var(--lux-status-warning)");
    expect(lightThemeRule(".status-failed")).toContain("color: var(--lux-status-danger)");
    expect(lightThemeRule(".lux-user-badge.is-ok")).toContain("color: var(--lux-status-success)");
    expect(lightThemeRule(".lux-user-badge.is-warn")).toContain("color: var(--lux-status-warning)");
    expect(lightThemeRule(".lux-notification-result")).toContain("color: var(--lux-status-success)");
    expect(lightThemeRule(".lux-job-icon.is-active")).toContain("color: var(--lux-status-active)");
  });

  it("uses light gradients for loading skeletons", () => {
    const skeleton = lightThemeRule(".lux-skeleton-block");
    expect(skeleton).toContain("background: linear-gradient(");
    expect(skeleton).not.toContain("#101014");
    expect(lightThemeRule(".lux-skeleton-row")).toContain("background: linear-gradient(");
    expect(lightThemeRule(".lux-skeleton-line")).toContain("background: linear-gradient(");
    expect(lightThemeRule(".lux-library-page-skeleton-card")).toContain("background: linear-gradient(");
    expect(lightThemeRule(".lux-library-page-skeleton-card")).not.toContain("#101014");
  });

  it("uses light surfaces for missing media and library covers", () => {
    expect(lightThemeRule(".lux-media-placeholder")).toContain("color: var(--lux-muted)");
    expect(lightThemeRule(".lux-media-placeholder")).toContain("linear-gradient(");
    expect(lightThemeRule(".lux-media-placeholder")).not.toContain("#27272c");
    expect(lightThemeRule(".lux-admin-library-cover-placeholder")).toContain("color: var(--lux-subtle)");
    expect(lightThemeRule(".lux-admin-library-cover-placeholder")).toContain("linear-gradient(");
    expect(lightThemeRule(".lux-admin-library-cover-placeholder")).not.toContain("#27272c");
  });

  it("uses dark focus rings on light surfaces while retaining the player ring", () => {
    expect(cssRule(":root")).toContain("--lux-focus-ring: #fff");
    expect(lightThemeRootRule()).toContain("--lux-focus-ring: #25252a");
    expect(cssRule(":focus-visible")).toContain("outline: 2px solid var(--lux-focus-ring)");
    expect(lightThemeRule(".lux-library-strategy-toggle input:focus-visible + span + i")).toContain("outline: 2px solid var(--lux-focus-ring)");
    expect(lightThemeRule(".lux-thumbnail-scraping-mode input:focus-visible + span")).toContain("outline: 2px solid var(--lux-focus-ring)");
    expect(stylesheet).toContain(".lux-player-center-play:focus-visible { outline: 3px solid rgba(255,255,255,.78)");
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
    expect(lightThemeRule(".lux-select-trigger")).toContain("background: rgba(28,28,34");
    expect(lightThemeRule('.lux-select-option[aria-selected="true"]')).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-media-info-row")).toContain("background: rgba(255,255,255");
    expect(lightThemeRule(".lux-media-info-row > strong")).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-media-stream-card")).toContain("background: rgba(255,255,255");
    expect(lightThemeRule(".lux-media-stream-heading h3")).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-season-tab.is-active")).toContain("color: var(--lux-text)");
    expect(lightThemeRule(".lux-episode-link")).toContain("background: rgba(255,255,255");
  });
});
