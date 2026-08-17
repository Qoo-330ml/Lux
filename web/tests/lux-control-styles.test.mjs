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

test("text field rules do not reshape checkbox controls", async () => {
  const notificationsCss = await readFile(new URL("../src/features/admin/notifications.css", import.meta.url), "utf8");
  const pluginCss = await readFile(new URL("../src/features/admin/plugin-library.css", import.meta.url), "utf8");

  for (const css of [notificationsCss, pluginCss]) {
    assert.match(css, /input:not\(\[type="checkbox"\]\):not\(\[type="radio"\]\)/);
  }
  assert.doesNotMatch(notificationsCss, /\.lux-notification-form input,\s*\.lux-notification-form select/);
  assert.doesNotMatch(pluginCss, /\.lux-admin-plugin-dialog-form input\s*\{/);
  assert.match(css, /\.lux-admin-form input:not\(\[type="checkbox"\]\):not\(\[type="radio"\]\)/);
  assert.match(css, /\.lux-admin-library-page \.lux-library-dialog input:not\(\[type="checkbox"\]\):not\(\[type="radio"\]\)/);
});

test("checked checkbox keeps its rotated checkmark", () => {
  assert.match(css, /:where\(input\[type="checkbox"\]\):checked::after\s*\{[^}]*transform:\s*translateY\(-1px\) rotate\(-45deg\) scale\(1\)/s);
});
