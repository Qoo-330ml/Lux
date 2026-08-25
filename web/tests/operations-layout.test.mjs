import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

const stylesheet = fs.readFileSync(new URL("../src/react.css", import.meta.url), "utf8");

function rule(selector) {
  return stylesheet.match(new RegExp(`${selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\s*\\{([^}]*)\\}`))?.[1] ?? "";
}

test("operations page fills the available admin content width", () => {
  const operationsRule = rule(".lux-admin-operations-page");
  const changelogRule = rule(".lux-changelog-page");

  assert.match(operationsRule, /width:\s*100%/);
  assert.match(operationsRule, /max-width:\s*none/);
  assert.doesNotMatch(operationsRule, /max-width:\s*1040px/);
  assert.match(changelogRule, /width:\s*100%/);
  assert.match(changelogRule, /max-width:\s*none/);
  assert.doesNotMatch(changelogRule, /max-width:\s*940px/);
});

test("registered task actions share the compact button height", () => {
  const editRule = rule(".lux-registered-task-edit");

  assert.match(editRule, /min-width:\s*var\(--lux-button-height-compact\)/);
  assert.match(editRule, /width:\s*var\(--lux-button-height-compact\)/);
  assert.match(editRule, /min-height:\s*var\(--lux-button-height-compact\)/);
  assert.match(editRule, /height:\s*var\(--lux-button-height-compact\)/);
  assert.match(editRule, /margin-top:\s*0/);
});

test("task schedule editing keeps the field and actions in one aligned row", () => {
  const editorRule = rule(".lux-registered-task-editor");
  const actionRule = rule(".lux-registered-task-editor-actions");
  const actionIconRule = rule(".lux-registered-task-editor-actions .lux-icon-button-small");
  const helpRule = rule(".lux-registered-task-editor-help");

  assert.match(editorRule, /grid-template-columns:\s*minmax\(180px,\s*1fr\)\s+auto\s+auto/);
  assert.match(editorRule, /align-items:\s*end/);
  assert.match(actionRule, /height:\s*var\(--lux-button-height\)/);
  assert.match(actionIconRule, /min-width:\s*var\(--lux-button-height\)/);
  assert.match(actionIconRule, /width:\s*var\(--lux-button-height\)/);
  assert.match(actionIconRule, /height:\s*var\(--lux-button-height\)/);
  assert.match(helpRule, /grid-column:\s*1\s*\/\s*-1/);
});

test("mobile registered task details keep the action row from narrowing the lower copy", () => {
  const mobileRules = stylesheet.match(/@media \(max-width: 900px\) \{([\s\S]*?)\n\}/)?.[1] ?? "";

  assert.match(mobileRules, /\.lux-registered-task-row\s*\{[^}]*position:\s*relative;[^}]*grid-template-columns:\s*38px\s+minmax\(0,\s*1fr\)/);
  assert.match(mobileRules, /\.lux-registered-task-row:not\(.is-editing\) \.lux-registered-task-heading\s*\{[^}]*padding-right:\s*148px/);
  assert.match(mobileRules, /\.lux-registered-task-actions\s*\{[^}]*position:\s*absolute;[^}]*top:\s*17px;[^}]*right:\s*15px;/);
});
