import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");

test("admin console uses the account settings page rhythm", () => {
  const layoutRule = stylesheet.match(/\.lux-admin-layout\s*\{([^}]*)\}/)?.[1] ?? "";
  const sidebarRule = stylesheet.match(/\.lux-admin-sidebar\s*\{([^}]*)\}/)?.[1] ?? "";
  const contentRule = stylesheet.match(/\.lux-admin-content\s*\{([^}]*)\}/)?.[1] ?? "";
  const headingRule = stylesheet.match(/\.lux-admin-page-heading\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(layoutRule, /width:\s*min\(1400px,\s*calc\(100%\s*-\s*48px\)\)/);
  assert.match(layoutRule, /padding:\s*var\(--lux-page-top\)\s+0\s+90px/);
  assert.match(sidebarRule, /background:\s*transparent/);
  assert.match(sidebarRule, /border-right:\s*0/);
  assert.match(contentRule, /padding:\s*0\s+0\s+90px\s+clamp\(28px,\s*5vw,\s*84px\)/);
  assert.match(headingRule, /margin-bottom:\s*42px/);
});

test("dashboard metrics and panels use separators instead of card chrome", () => {
  const statsRule = stylesheet.match(/\.lux-admin-stat\s*\{([^}]*)\}/)?.[1] ?? "";
  const panelRule = stylesheet.match(/\.lux-admin-panel\s*\{([^}]*)\}/)?.[1] ?? "";
  const panelGridRule = stylesheet.match(/\.lux-admin-dashboard-grid\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(statsRule, /border:\s*0/);
  assert.match(statsRule, /border-left:\s*1px\s+solid\s+var\(--lux-line-soft\)/);
  assert.match(statsRule, /background:\s*transparent/);
  assert.match(panelRule, /border:\s*0/);
  assert.match(panelRule, /border-radius:\s*0/);
  assert.match(panelRule, /background:\s*transparent/);
  assert.match(panelGridRule, /border-bottom:\s*1px\s+solid\s+var\(--lux-line-soft\)/);
});

test("light mode preserves the same flat admin surfaces", () => {
  const lightAdminStyles = stylesheet.slice(stylesheet.indexOf("/* The admin console is a normal light surface"));

  assert.match(lightAdminStyles, /html\[data-lux-theme="light"\] \.lux-admin-layout \{[^}]*background:\s*var\(--lux-bg\)/);
  assert.match(lightAdminStyles, /html\[data-lux-theme="light"\] \.lux-admin-sidebar \{[^}]*background:\s*transparent/);
  assert.match(lightAdminStyles, /html\[data-lux-theme="light"\] \.lux-admin-stat \{[^}]*background:\s*transparent/);
  assert.match(lightAdminStyles, /html\[data-lux-theme="light"\] \.lux-admin-panel \{[^}]*background:\s*transparent/);
});

test("now-playing cards use theme tokens and compact proportions", () => {
  const cardRule = stylesheet.match(/\.lux-now-playing-card\s*\{([^}]*)\}/)?.[1] ?? "";
  const bodyRule = stylesheet.match(/\.lux-now-playing-body\s*\{([^}]*)\}/)?.[1] ?? "";
  const lightRule = stylesheet.match(/html\[data-lux-theme="light"\] \.lux-now-playing-card\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(cardRule, /background:\s*var\(--lux-now-card-bg\)/);
  assert.match(bodyRule, /padding:\s*16px\s+20px/);
  assert.match(bodyRule, /minmax\(84px,\s*9%\)/);
  assert.match(lightRule, /--lux-now-card-bg:\s*#fbfcfe/);
});
