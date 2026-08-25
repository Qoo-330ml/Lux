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
