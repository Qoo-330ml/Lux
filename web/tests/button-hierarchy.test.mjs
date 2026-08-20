import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

const stylesheet = fs.readFileSync(new URL("../src/react.css", import.meta.url), "utf8");

function rule(selector) {
  return stylesheet.match(new RegExp(`${selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\s*\\{([^}]*)\\}`))?.[1] ?? "";
}

test("global action buttons use named size tiers", () => {
  assert.match(stylesheet, /--lux-button-height-large:\s*48px/);
  assert.match(stylesheet, /--lux-button-height:\s*40px/);
  assert.match(stylesheet, /--lux-button-height-compact:\s*36px/);
  assert.match(stylesheet, /--lux-button-height-touch:\s*44px/);

  assert.match(rule(".lux-button"), /min-height:\s*var\(--lux-button-height\)/);
  assert.match(rule(".lux-button-large"), /min-height:\s*var\(--lux-button-height-large\)/);
  assert.match(rule(".lux-button-compact"), /min-height:\s*var\(--lux-button-height-compact\)/);
  assert.match(rule(".lux-button-touch"), /min-height:\s*var\(--lux-button-height-touch\)/);
});

test("global button overrides do not introduce one-off action heights", () => {
  for (const selector of [
    ".lux-media-editor-footer .lux-button",
    ".lux-image-editor-toolbar .lux-button",
    ".lux-admin-server-form .lux-button",
    ".lux-log-export-controls .lux-button",
    ".lux-account-api-key-actions .lux-button",
    ".lux-account-page .lux-button",
  ]) {
    assert.doesNotMatch(rule(selector), /min-height:/, selector);
  }

  assert.match(rule(".lux-library-toolbar-button"), /min-height:\s*var\(--lux-button-height-compact\)/);
  assert.match(rule(".lux-library-action-menu button"), /min-height:\s*var\(--lux-button-height\)/);
});
