import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

test("detail poster and copy share a top alignment baseline", () => {
  const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");
  const detailGridRule = stylesheet.match(/\.lux-detail-grid\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(detailGridRule, /align-items:\s*start/);
});
