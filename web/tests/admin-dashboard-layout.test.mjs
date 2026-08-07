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

test("dashboard panels use separators instead of card chrome", () => {
  const panelRule = stylesheet.match(/\.lux-admin-panel\s*\{([^}]*)\}/)?.[1] ?? "";
  const panelGridRule = stylesheet.match(/\.lux-admin-dashboard-grid\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(panelRule, /border:\s*0/);
  assert.match(panelRule, /border-radius:\s*0/);
  assert.match(panelRule, /background:\s*transparent/);
  assert.match(panelGridRule, /border-bottom:\s*1px\s+solid\s+var\(--lux-line-soft\)/);
});

test("light mode preserves the same flat admin surfaces", () => {
  const lightAdminStyles = stylesheet.slice(stylesheet.indexOf("/* The admin console is a normal light surface"));

  assert.match(lightAdminStyles, /html\[data-lux-theme="light"\] \.lux-admin-layout \{[^}]*background:\s*var\(--lux-bg\)/);
  assert.match(lightAdminStyles, /html\[data-lux-theme="light"\] \.lux-admin-sidebar \{[^}]*background:\s*transparent/);
  assert.match(lightAdminStyles, /html\[data-lux-theme="light"\] \.lux-admin-panel \{[^}]*background:\s*transparent/);
});

test("now-playing cards use theme tokens and compact proportions", () => {
  const cardRule = stylesheet.match(/\.lux-now-playing-card\s*\{([^}]*)\}/)?.[1] ?? "";
  const gridRule = stylesheet.match(/\.lux-now-playing-grid\s*\{([^}]*)\}/)?.[1] ?? "";
  const bodyRule = stylesheet.match(/\.lux-now-playing-body\s*\{([^}]*)\}/)?.[1] ?? "";
  const factsRule = stylesheet.match(/\.lux-now-playing-facts\s*\{([^}]*)\}/)?.[1] ?? "";
  const networkRule = stylesheet.match(/\.lux-now-playing-network\s*\{([^}]*)\}/)?.[1] ?? "";
  const clientRule = stylesheet.match(/\.lux-now-playing-client\s*\{([^}]*)\}/)?.[1] ?? "";
  const networkFieldRule = stylesheet.match(/\.lux-now-playing-network-field\s*\{([^}]*)\}/)?.[1] ?? "";
  const accountRule = stylesheet.match(/\.lux-now-playing-account\s*\{([^}]*)\}/)?.[1] ?? "";
  const factCopyRule = stylesheet.match(/\.lux-now-playing-fact-copy\s*\{([^}]*)\}/)?.[1] ?? "";
  const lightRule = stylesheet.match(/html\[data-lux-theme="light"\] \.lux-now-playing-card\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(cardRule, /background:\s*var\(--lux-now-card-bg\)/);
  assert.match(gridRule, /width:\s*min\(100%,\s*480px\)/);
  assert.match(bodyRule, /gap:\s*14px/);
  assert.match(bodyRule, /padding:\s*12px\s+16px/);
  assert.match(bodyRule, /minmax\(84px,\s*9%\)/);
  assert.match(factsRule, /display:\s*flex/);
  assert.match(factsRule, /flex-direction:\s*column/);
  assert.match(factCopyRule, /display:\s*flex/);
  assert.match(factCopyRule, /align-items:\s*baseline/);
  assert.match(factsRule, /background:\s*transparent/);
  assert.match(networkRule, /background:\s*transparent/);
  assert.match(clientRule, /border-left:\s*0/);
  assert.match(networkFieldRule, /padding:\s*5px\s+0/);
  assert.match(accountRule, /flex-direction:\s*column/);
  assert.doesNotMatch(factsRule, /border-top:\s*1px/);
  assert.doesNotMatch(networkRule, /border-top:\s*1px/);
  assert.doesNotMatch(stylesheet, /\.lux-now-playing-fact \+ \.lux-now-playing-fact \{[^}]*border-left:\s*1px/);
  assert.doesNotMatch(stylesheet, /\.lux-now-playing-network-field \+ \.lux-now-playing-network-field \{[^}]*border-left:\s*1px/);
  assert.match(lightRule, /--lux-now-card-bg:\s*#fbfcfe/);
});
