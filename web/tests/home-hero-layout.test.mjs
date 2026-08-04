import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";

const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");

test("hero carousel controls share a flexible row and stay at its right edge", () => {
  const actionRowRule = stylesheet.match(/\.lux-hero-action-row\s*\{([^}]*)\}/)?.[1] ?? "";
  const carouselRule = stylesheet.match(/\.lux-hero-carousel-controls\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(actionRowRule, /display:\s*flex/);
  assert.match(actionRowRule, /align-items:\s*center/);
  assert.match(actionRowRule, /flex-wrap:\s*wrap/);
  assert.match(carouselRule, /margin-left:\s*auto/);
  assert.match(carouselRule, /margin-top:\s*0/);
});

test("hero keeps a shorter vertical footprint across responsive breakpoints", () => {
  assert.match(stylesheet, /\.lux-hero\s*\{[^}]*min-height:\s*min\(82vh,\s*820px\)/);
  assert.match(stylesheet, /@media \(max-width: 900px\) \{[\s\S]*?\.lux-hero\s*\{\s*min-height:\s*680px/);
  assert.match(stylesheet, /@media \(max-width: 560px\) \{[\s\S]*?\.lux-hero\s*\{\s*min-height:\s*620px/);
});

test("hero logos fit inside the title area without distortion", () => {
  const logoRule = stylesheet.match(/\.lux-hero-logo\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(logoRule, /max-height:\s*145px/);
  assert.match(logoRule, /object-fit:\s*contain/);
  assert.match(logoRule, /object-position:\s*left\s+center/);
});
