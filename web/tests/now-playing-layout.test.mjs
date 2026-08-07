import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");

test("now-playing cards use fixed compact columns and expandable episode subtitles", () => {
  const headingRule = stylesheet.match(/\.lux-now-playing-heading\s*\{([^}]*)\}/)?.[1] ?? "";
  const gridRule = stylesheet.match(/\.lux-now-playing-grid\s*\{([^}]*)\}/)?.[1] ?? "";
  const subtitleRule = stylesheet.match(/\.lux-now-playing-subtitle\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(headingRule, /display:\s*grid/);
  assert.match(headingRule, /grid-template-columns:\s*minmax\(0,\s*1fr\)\s+auto/);
  assert.match(gridRule, /grid-template-columns:\s*repeat\(auto-fit,\s*minmax\(min\(100%,\s*288px\),\s*288px\)\)/);
  assert.match(gridRule, /width:\s*100%/);
  assert.match(subtitleRule, /display:\s*-webkit-box/);
  assert.match(subtitleRule, /grid-column:\s*1\s*\/\s*-1/);
  assert.match(subtitleRule, /-webkit-line-clamp:\s*2/);
  assert.match(subtitleRule, /white-space:\s*normal/);
});
