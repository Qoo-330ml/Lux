import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");
const responsiveWidth = /width:\s*92%/;
const fixedPixelWidth = /width:\s*(?:\d+px|min\(\s*\d+px)/;

function rule(selector) {
  const escapedSelector = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return stylesheet.match(new RegExp(`${escapedSelector}\\s*\\{([^}]*)\\}`))?.[1] ?? "";
}

test("primary page surfaces use viewport-relative widths on large displays", () => {
  for (const selector of [
    ".lux-home-content",
    ".lux-page",
    ".lux-page-narrow",
    ".lux-detail-content",
    ".lux-player-topbar",
    ".lux-player-frame",
    ".lux-admin-layout",
  ]) {
    assert.match(rule(selector), responsiveWidth, selector);
  }
});

test("large-display page surfaces do not keep fixed pixel width caps", () => {
  for (const selector of [
    ".lux-home-content",
    ".lux-page",
    ".lux-page-narrow",
    ".lux-detail-content",
    ".lux-player-topbar",
    ".lux-player-frame",
    ".lux-admin-layout",
  ]) {
    assert.doesNotMatch(rule(selector), fixedPixelWidth, selector);
  }
});

test("the global header stays fixed while page content scrolls", () => {
  assert.match(rule(".lux-header"), /position:\s*fixed/);
  assert.match(rule(".lux-header"), /(?:top:\s*0|inset:\s*0\s+0\s+auto)/);
});

test("mobile navigation stays attached below the fixed header", () => {
  const fixedMobileNavRule = stylesheet.match(/\.lux-mobile-nav\s*\{\s*position:\s*fixed[^}]*\}/)?.[0] ?? "";

  assert.match(fixedMobileNavRule, /position:\s*fixed/);
  assert.match(fixedMobileNavRule, /top:\s*var\(--lux-header-height\)/);
});
