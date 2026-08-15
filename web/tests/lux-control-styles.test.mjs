import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const css = await readFile(new URL("../src/react.css", import.meta.url), "utf8");

test("defines shared Lux styles for form inputs and checkboxes", () => {
  assert.match(css, /:where\(input:not\(\[type="checkbox"\]\)/);
  assert.match(css, /:where\(input\[type="checkbox"\]\)/);
  assert.match(css, /input\[type="checkbox"\]\):checked::after/);
});

test("plugin configuration uses LuxSelect for all select fields", async () => {
  const source = await readFile(new URL("../src/features/admin/AdminPluginsPage.tsx", import.meta.url), "utf8");
  assert.doesNotMatch(source, /<select\b/);
  assert.match(source, /<LuxSelect[\s\S]*multiple/);
});
