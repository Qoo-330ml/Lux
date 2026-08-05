import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");
const accountStyles = stylesheet.slice(stylesheet.indexOf("/* Account settings */"));

test("account settings content stays centered as one layout", () => {
  const settingsGridRule = stylesheet.match(/\.lux-account-settings-grid\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(settingsGridRule, /justify-content:\s*center/);
  assert.doesNotMatch(settingsGridRule, /justify-content:\s*space-between/);
});

test("account settings keeps the sidebar left and sync status aligned with the cards", () => {
  const pageRule = accountStyles.match(/\.lux-account-page\s*\{([^}]*)\}/)?.[1] ?? "";
  const headingRule = accountStyles.match(/\.lux-account-page-heading\s*\{([^}]*)\}/)?.[1] ?? "";
  const sidebarRule = accountStyles.match(/\.lux-account-settings-sidebar\s*\{([^}]*)\}/)?.[1] ?? "";
  const mobileSidebarRule = accountStyles.match(/@media \(max-width: 900px\) \{[\s\S]*?\.lux-account-settings-sidebar\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(pageRule, /--lux-account-settings-gap:\s*clamp\(28px,\s*5vw,\s*84px\)/);
  assert.match(headingRule, /margin-right:\s*max\(0px,/);
  assert.match(sidebarRule, /transform:\s*translateX\(calc\(-1\s*\*\s*clamp\(24px,\s*3vw,\s*48px\)\)\)/);
  assert.match(mobileSidebarRule, /transform:\s*none/);
});
