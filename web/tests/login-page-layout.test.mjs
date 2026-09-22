import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");

test("login poster waterfall keeps the requested scale, inset, and stagger", () => {
  const waterfallRule = stylesheet.match(/\.lux-auth-poster-waterfall\s*\{([^}]*)\}/)?.[1] ?? "";
  const middleColumnRule = stylesheet.match(/\.lux-auth-poster-waterfall-column:nth-child\(2\)\s*\{([^}]*)\}/)?.[1] ?? "";
  const lastColumnRule = stylesheet.match(/\.lux-auth-poster-waterfall-column:nth-child\(3\)\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(waterfallRule, /left:\s*16%/);
  assert.match(waterfallRule, /right:\s*0%/);
  assert.match(waterfallRule, /transform:\s*rotate\(6deg\)\s*scale\(\.78\)/);
  assert.match(waterfallRule, /transform-origin:\s*left center/);
  assert.match(middleColumnRule, /padding-top:\s*9%/);
  assert.match(lastColumnRule, /padding-top:\s*4%/);
});
