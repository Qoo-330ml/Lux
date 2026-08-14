import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");
const accountStyles = stylesheet.slice(stylesheet.indexOf("/* Account settings */"));

test("account settings uses the full viewport-relative page surface on large displays", () => {
  const pageRule = stylesheet.match(/\.lux-account-page-heading\s*\{([^}]*)\}/)?.[1] ?? "";
  const settingsGridRule = stylesheet.match(/\.lux-account-settings-grid\s*\{([^}]*)\}/)?.[1] ?? "";
  const sidebarRule = stylesheet.match(/\.lux-account-settings-sidebar\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(pageRule, /margin-right:\s*0/);
  assert.match(settingsGridRule, /grid-template-columns:\s*220px\s+minmax\(0,\s*1fr\)/);
  assert.match(settingsGridRule, /transform:\s*none/);
  assert.match(sidebarRule, /transform:\s*none/);
  assert.doesNotMatch(settingsGridRule, /minmax\(0,\s*760px\)/);
  assert.doesNotMatch(settingsGridRule, /justify-content:\s*center/);
});

test("account settings sections use separators instead of card chrome", () => {
  const contentRule = accountStyles.match(/\.lux-account-settings-content\s*\{([^}]*)\}/)?.[1] ?? "";
  const sectionRule = accountStyles.match(/\.lux-account-settings-section\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(contentRule, /gap:\s*0/);
  assert.match(sectionRule, /border:\s*0/);
  assert.match(sectionRule, /border-radius:\s*0/);
  assert.match(sectionRule, /background:\s*transparent/);
  assert.match(accountStyles, /\.lux-account-settings-section\s*\+\s*\.lux-account-settings-section\s*\{[^}]*margin-top:\s*28px[^}]*border-top:\s*1px\s+solid\s+var\(--lux-settings-line\)/);
  assert.doesNotMatch(accountStyles, /html\[data-lux-theme="light"\]\s+\.lux-account-settings-section/);
});

test("account settings keeps the sidebar left and sync status aligned across the page surface", () => {
  const pageRule = accountStyles.match(/\.lux-account-page\s*\{([^}]*)\}/)?.[1] ?? "";
  const headingRule = accountStyles.match(/\.lux-account-page-heading\s*\{([^}]*)\}/)?.[1] ?? "";
  const headingTitleRule = accountStyles.match(/\.lux-account-page-heading h1\s*\{([^}]*)\}/)?.[1] ?? "";
  const sidebarRule = accountStyles.match(/\.lux-account-settings-sidebar\s*\{([^}]*)\}/)?.[1] ?? "";
  const mobileSidebarRule = accountStyles.match(/@media \(max-width: 900px\) \{[\s\S]*?\.lux-account-settings-sidebar\s*\{([^}]*)\}/)?.[1] ?? "";
  const mobileGridRule = accountStyles.match(/@media \(max-width: 900px\) \{\s*\.lux-account-settings-grid\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(pageRule, /--lux-account-settings-gap:\s*clamp\(28px,\s*5vw,\s*84px\)/);
  assert.match(headingRule, /margin-right:\s*0/);
  assert.match(headingTitleRule, /font-size:\s*clamp\(1\.75rem,\s*3vw,\s*2\.6rem\)/);
  assert.match(sidebarRule, /transform:\s*none/);
  assert.match(mobileGridRule, /transform:\s*none/);
  assert.match(mobileSidebarRule, /transform:\s*none/);
});
