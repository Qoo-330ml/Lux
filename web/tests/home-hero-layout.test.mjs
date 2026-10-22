import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { stylesheet as allStyles } from "./style-source.mjs";

const stylesheet = allStyles;

test("hero carousel controls share a flexible row and stay at its right edge", () => {
  const actionRowRule = stylesheet.match(/\.lux-hero-action-row\s*\{([^}]*)\}/)?.[1] ?? "";
  const carouselRule = stylesheet.match(/\.lux-hero-carousel-controls\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(actionRowRule, /display:\s*flex/);
  assert.match(actionRowRule, /align-items:\s*center/);
  assert.match(actionRowRule, /flex-wrap:\s*wrap/);
  assert.match(actionRowRule, /width:\s*calc\(100vw\s*-\s*clamp\(24px,\s*7\.4vw,\s*150px\)\s*-\s*48px\)/);
  assert.match(carouselRule, /margin-left:\s*auto/);
  assert.match(carouselRule, /margin-top:\s*0/);
});

test("hero carousel uses a bordered progress dot instead of arrow controls", () => {
  assert.doesNotMatch(stylesheet, /\.lux-hero-carousel-arrow/);
  assert.match(stylesheet, /\.lux-hero-dot\.is-active\s*\{[^}]*border:\s*1px\s+solid/);
  assert.match(stylesheet, /\.lux-hero-dot-progress\s*\{[^}]*animation:\s*lux-hero-dot-progress/);
  assert.match(stylesheet, /@keyframes lux-hero-dot-progress\s*\{[\s\S]*?100%\s*\{[^}]*transform:\s*scaleX\(1\)/);
});

test("hero keeps a shorter vertical footprint across responsive breakpoints", () => {
  assert.match(stylesheet, /\.lux-hero\s*\{[^}]*min-height:\s*min\(82vh,\s*820px\)/);
  assert.match(stylesheet, /@media \(max-width: 900px\) \{[\s\S]*?\.lux-hero\s*\{\s*min-height:\s*680px/);
  assert.match(stylesheet, /@media \(max-width: 560px\) \{[\s\S]*?\.lux-hero\s*\{\s*min-height:\s*620px/);
});

test("hero action row returns to the copy width on small screens", () => {
  assert.match(stylesheet, /@media \(max-width: 560px\) \{[\s\S]*?\.lux-hero-action-row\s*\{[^}]*width:\s*100%/);
});

test("hero logos fit inside the title area without distortion", () => {
  const logoRule = stylesheet.match(/\.lux-hero-logo\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(logoRule, /max-height:\s*145px/);
  assert.match(logoRule, /object-fit:\s*contain/);
  assert.match(logoRule, /object-position:\s*left\s+center/);
});

test("hero text titles reserve three lines and clamp overflow", () => {
  const titleRule = stylesheet.match(/\.lux-hero-title\s*\{([^}]*)\}/)?.[1] ?? "";
  const titleTextRule = stylesheet.match(/\.lux-hero-title-text\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(titleRule, /--lux-hero-title-line-height:\s*\.94em/);
  assert.match(titleRule, /min-height:\s*calc\(3\s*\*\s*var\(--lux-hero-title-line-height\)\)/);
  assert.match(titleTextRule, /-webkit-line-clamp:\s*3/);
  assert.match(titleTextRule, /overflow-wrap:\s*anywhere/);
});

test("hero text titles provide two smaller readable size levels", () => {
  assert.match(stylesheet, /\.lux-hero-title--compact\s*\{[^}]*font-size:/);
  assert.match(stylesheet, /\.lux-hero-title--small\s*\{[^}]*font-size:/);
});

test("hero slide copy and backdrops use CSS animations with reduced-motion support", () => {
  const backdropRule = stylesheet.match(/\.lux-hero-backdrop-animated\s*\{([^}]*)\}/)?.[1] ?? "";
  const copyRule = stylesheet.match(/\.lux-hero-copy-animated\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(backdropRule, /animation:\s*lux-hero-backdrop-in\s*\.55s\s*ease-out\s*both/);
  assert.match(copyRule, /animation:\s*lux-hero-copy-in\s*\.38s\s*ease\s*both/);
  assert.match(stylesheet, /@keyframes lux-hero-backdrop-in\s*\{\s*from\s*\{[^}]*opacity:\s*0[^}]*transform:\s*scale\(1\.04\)[^}]*\}\s*to\s*\{[^}]*opacity:\s*1[^}]*transform:\s*scale\(1\.015\)/);
  assert.match(stylesheet, /@keyframes lux-hero-copy-in\s*\{\s*from\s*\{[^}]*opacity:\s*0[^}]*translateY\(18px\)[^}]*\}\s*to\s*\{[^}]*opacity:\s*1[^}]*translateY\(0\)/);
  assert.match(stylesheet, /@media \(prefers-reduced-motion: reduce\) \{\s*\*, \*::before, \*::after \{[^}]*animation-duration:\s*\.01ms !important;[^}]*transition-duration:\s*\.01ms !important;/);
});

test("login panel entrance uses the same reduced-motion-safe CSS path", () => {
  const authCardRule = stylesheet.match(/\.lux-auth-card-animated\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(authCardRule, /animation:\s*lux-auth-card-in\s*\.35s\s*ease\s*both/);
  assert.match(stylesheet, /@keyframes lux-auth-card-in\s*\{\s*from\s*\{[^}]*opacity:\s*0[^}]*translateY\(16px\)[^}]*\}\s*to\s*\{[^}]*opacity:\s*1[^}]*translateY\(0\)/);
});
