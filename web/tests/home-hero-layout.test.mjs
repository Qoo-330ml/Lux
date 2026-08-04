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
