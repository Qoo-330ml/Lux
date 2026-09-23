import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");

test("login poster waterfall keeps the requested scale, inset, and stagger", () => {
  const waterfallRule = stylesheet.match(/\.lux-auth-poster-waterfall\s*\{([^}]*)\}/)?.[1] ?? "";
  const middleColumnRule = stylesheet.match(/\.lux-auth-poster-waterfall-column:nth-child\(2\)\s*\{([^}]*)\}/)?.[1] ?? "";
  const thirdColumnRule = stylesheet.match(/\.lux-auth-poster-waterfall-column:nth-child\(3\)\s*\{([^}]*)\}/)?.[1] ?? "";
  const fourthColumnRule = stylesheet.match(/\.lux-auth-poster-waterfall-column:nth-child\(4\)\s*\{([^}]*)\}/)?.[1] ?? "";
  const fifthColumnRule = stylesheet.match(/\.lux-auth-poster-waterfall-column:nth-child\(5\)\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(waterfallRule, /left:\s*20%/);
  assert.match(waterfallRule, /right:\s*-4%/);
  assert.match(waterfallRule, /grid-template-columns:\s*repeat\(5,\s*minmax\(0,\s*var\(--lux-auth-poster-column-width,\s*26%\)\)\)/);
  assert.match(waterfallRule, /column-gap:\s*12px/);
  assert.match(waterfallRule, /row-gap:\s*12px/);
  assert.match(waterfallRule, /justify-content:\s*start/);
  assert.match(waterfallRule, /transform:\s*rotate\(6deg\)/);
  assert.match(waterfallRule, /transform-origin:\s*left center/);
  assert.match(stylesheet, /\.lux-auth-poster-waterfall-column\s*\{[^}]*align-items:\s*flex-end[^}]*gap:\s*12px/);
  assert.match(middleColumnRule, /margin-top:\s*-15%/);
  assert.match(thirdColumnRule, /margin-top:\s*-30%/);
  assert.match(fourthColumnRule, /margin-top:\s*-45%/);
  assert.match(fifthColumnRule, /margin-top:\s*-60%/);
  assert.match(stylesheet, /\.lux-auth-poster-waterfall img\s*\{[^}]*width:\s*100%/);
});
