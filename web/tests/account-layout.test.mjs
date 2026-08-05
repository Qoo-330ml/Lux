import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");

test("account settings content stays centered as one layout", () => {
  const settingsGridRule = stylesheet.match(/\.lux-account-settings-grid\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(settingsGridRule, /justify-content:\s*center/);
  assert.doesNotMatch(settingsGridRule, /justify-content:\s*space-between/);
});
