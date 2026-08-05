import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");
const accountStyles = stylesheet.slice(stylesheet.indexOf("/* Account settings */"));

test("account settings keeps its centered layout while moving left on desktop", () => {
  const settingsGridRule = stylesheet.match(/\.lux-account-settings-grid\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(settingsGridRule, /justify-content:\s*center/);
  assert.match(settingsGridRule, /transform:\s*translateX\(calc\(-1\s*\*\s*clamp\(20px,\s*3vw,\s*48px\)\)\)/);
  assert.doesNotMatch(settingsGridRule, /justify-content:\s*space-between/);
});

test("account settings sections use separators instead of card chrome", () => {
  const contentRule = accountStyles.match(/\.lux-account-settings-content\s*\{([^}]*)\}/)?.[1] ?? "";
  const sectionRule = accountStyles.match(/\.lux-account-settings-section\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(contentRule, /gap:\s*0/);
  assert.match(sectionRule, /border:\s*0/);
  assert.match(sectionRule, /border-radius:\s*0/);
  assert.match(sectionRule, /background:\s*transparent/);
  assert.match(accountStyles, /\.lux-account-settings-section\s*\+\s*\.lux-account-settings-section\s*\{[^}]*border-top:\s*1px\s+solid\s+var\(--lux-settings-line\)/);
  assert.doesNotMatch(accountStyles, /html\[data-lux-theme="light"\]\s+\.lux-account-settings-section/);
});

test("account settings keeps the sidebar left and sync status aligned with the cards", () => {
  const pageRule = accountStyles.match(/\.lux-account-page\s*\{([^}]*)\}/)?.[1] ?? "";
  const headingRule = accountStyles.match(/\.lux-account-page-heading\s*\{([^}]*)\}/)?.[1] ?? "";
  const sidebarRule = accountStyles.match(/\.lux-account-settings-sidebar\s*\{([^}]*)\}/)?.[1] ?? "";
  const mobileSidebarRule = accountStyles.match(/@media \(max-width: 900px\) \{[\s\S]*?\.lux-account-settings-sidebar\s*\{([^}]*)\}/)?.[1] ?? "";
  const mobileGridRule = accountStyles.match(/@media \(max-width: 900px\) \{\s*\.lux-account-settings-grid\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(pageRule, /--lux-account-settings-gap:\s*clamp\(28px,\s*5vw,\s*84px\)/);
  assert.match(headingRule, /margin-right:\s*max\(0px,/);
  assert.match(headingRule, /clamp\(20px,\s*3vw,\s*48px\)/);
  assert.match(sidebarRule, /transform:\s*translateX\(calc\(-1\s*\*\s*clamp\(24px,\s*3vw,\s*48px\)\)\)/);
  assert.match(mobileGridRule, /transform:\s*none/);
  assert.match(mobileSidebarRule, /transform:\s*none/);
});
