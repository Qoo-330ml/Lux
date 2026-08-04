import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

test("detail logo and title stack vertically", () => {
  const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");
  const titleRowRule = stylesheet.match(/\.lux-detail-title-row\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(titleRowRule, /flex-direction:\s*column/);
  assert.match(titleRowRule, /align-items:\s*flex-start/);
});
