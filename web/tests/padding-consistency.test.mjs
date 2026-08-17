import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");

function rule(selector) {
  const escapedSelector = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return stylesheet.match(new RegExp(`${escapedSelector}\\s*\\{([^}]*)\\}`))?.[1] ?? "";
}

test("shared control padding is used by form controls across pages", () => {
  assert.match(stylesheet, /--lux-control-padding-x:\s*12px/);

  for (const selector of [
    ".lux-select-trigger",
    ".lux-select-option",
    ".lux-media-action",
    ".lux-auth-form input:not([type=\"checkbox\"]):not([type=\"radio\"])",
    ".lux-admin-form input:not([type=\"checkbox\"]):not([type=\"radio\"]), .lux-admin-form select, .lux-admin-schedule-field input:not([type=\"checkbox\"]):not([type=\"radio\"]), .lux-admin-root-form input:not([type=\"checkbox\"]):not([type=\"radio\"])",
    ".lux-admin-server-form input:not([type=\"checkbox\"]):not([type=\"radio\"])",
    ".lux-admin-scraper-field select",
    ".lux-admin-password-row input",
    ".lux-admin-input-with-suffix input",
    ".lux-admin-settings-form > label > input[type=\"url\"]",
    ".lux-admin-filter-select .lux-select-trigger",
    ".lux-registered-task-editor input:not([type=\"checkbox\"])",
    ".lux-operations-log-toolbar input",
    ".lux-library-strategy-form-grid input:not([type=\"checkbox\"]):not([type=\"radio\"]), .lux-library-strategy-form-grid select, .lux-library-strategy-card input:not([type=\"checkbox\"]):not([type=\"radio\"]), .lux-library-strategy-card select",
    ".lux-library-override-fields input, .lux-library-override-fields select",
    ".lux-admin-library-page .lux-library-dialog input:not([type=\"checkbox\"]):not([type=\"radio\"]), .lux-admin-library-page .lux-library-dialog select",
    ".lux-setting-field input:not([type=\"checkbox\"]):not([type=\"radio\"]), .lux-setting-field select",
    ".lux-account-settings-nav a",
    ".lux-upload-button",
  ]) {
    assert.match(rule(selector), /padding:\s*0 var\(--lux-control-padding-x\)/, selector);
  }
});

test("same-page cards, buttons, and dialogs share their padding tokens", () => {
  assert.match(stylesheet, /--lux-button-padding-x:\s*16px/);
  assert.match(stylesheet, /--lux-card-copy-padding:\s*10px\s+0\s+0/);
  assert.match(stylesheet, /--lux-card-rail-padding:\s*16px\s+0\s+8px/);
  assert.match(stylesheet, /--lux-panel-padding:\s*20px/);
  assert.match(stylesheet, /--lux-dialog-header-padding:\s*20px\s+24px\s+16px/);
  assert.match(stylesheet, /--lux-dialog-content-padding:\s*20px\s+24px\s+24px/);

  for (const selector of [
    ".lux-button",
    ".lux-library-tabs button",
    ".lux-library-toolbar-button",
  ]) {
    assert.match(rule(selector), /padding:\s*0 var\(--lux-button-padding-x\)/, selector);
  }

  assert.match(rule(".lux-library-rail, .lux-media-rail"), /padding:\s*var\(--lux-card-rail-padding\)/);
  assert.match(rule(".lux-admin-panel"), /padding:\s*var\(--lux-panel-padding\)/);
  assert.match(rule(".lux-admin-library-heading"), /padding:\s*var\(--lux-panel-padding\)/);
  assert.match(rule(".lux-admin-library-body"), /padding:\s*var\(--lux-panel-padding\)/);
  assert.match(rule(".lux-admin-subpanel"), /padding:\s*var\(--lux-panel-padding\)/);

  for (const selector of [
    ".lux-library-card-copy",
    ".lux-continue-copy",
    ".lux-media-copy",
  ]) {
    assert.match(rule(selector), /padding:\s*var\(--lux-card-copy-padding\)/, selector);
  }

  for (const selector of [
    ".lux-media-editor-header",
    ".lux-detail-overview-dialog-header",
    ".lux-library-dialog-header",
  ]) {
    assert.match(rule(selector), /padding:\s*var\(--lux-dialog-header-padding\)/, selector);
  }

  for (const selector of [
    ".lux-metadata-editor-form",
    ".lux-image-editor-body",
    ".lux-detail-overview-dialog-body",
  ]) {
    assert.match(rule(selector), /padding:\s*var\(--lux-dialog-content-padding\)/, selector);
  }
});

test("dense editor and strategy controls keep their page-level horizontal padding", () => {
  assert.match(
    rule(".lux-metadata-field input:not([type=\"checkbox\"]):not([type=\"radio\"]), .lux-metadata-field textarea"),
    /padding:\s*10px var\(--lux-control-padding-x\)/,
  );
  assert.match(
    rule(".lux-library-strategy-toggle"),
    /padding:\s*10px var\(--lux-control-padding-x\)/,
  );
  assert.match(
    rule(".lux-library-override-toggles .lux-library-strategy-toggle"),
    /padding:\s*10px var\(--lux-control-padding-x\)/,
  );
  assert.doesNotMatch(stylesheet, /\.lux-admin-panel \{ padding:\s*17px; \}/);
});
