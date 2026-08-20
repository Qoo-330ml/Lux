import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

const stylesheet = fs.readFileSync(new URL("../src/react.css", import.meta.url), "utf8");

function rule(selector) {
  return stylesheet.match(new RegExp(`${selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\s*\\{([^}]*)\\}`))?.[1] ?? "";
}

test("server settings sections share one content width", () => {
  const panelRule = rule(".lux-admin-settings-panel");
  const formRule = rule(".lux-admin-settings-form");
  const noteRule = rule(".lux-admin-settings-note");

  assert.match(panelRule, /max-width:\s*760px/);
  assert.match(formRule, /width:\s*100%/);
  assert.match(formRule, /max-width:\s*760px/);
  assert.match(noteRule, /max-width:\s*760px/);
});
