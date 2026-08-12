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

test("the fixed header softens scrolling content behind its gradient", () => {
  const veil = rule(".lux-header::before");

  assert.match(rule(".lux-header"), /background:\s*transparent/);
  assert.match(veil, /background:\s*linear-gradient\(/);
  assert.match(veil, /backdrop-filter:\s*blur\(18px\)/);
  assert.match(veil, /-webkit-backdrop-filter:\s*blur\(18px\)/);
});

test("the header blur fades below the toolbar instead of ending on a hard edge", () => {
  const fade = rule(".lux-header::before");

  assert.match(fade, /inset:\s*0\s+0\s+auto/);
  assert.match(fade, /height:\s*calc\(100%\s*\+\s*clamp\(40px,\s*4vw,\s*64px\)\)/);
  assert.match(fade, /backdrop-filter:\s*blur\(18px\)/);
  assert.match(fade, /mask-image:\s*linear-gradient\(/);
  assert.match(fade, /pointer-events:\s*none/);
});

test("mobile navigation stays attached below the fixed header", () => {
  const fixedMobileNavRule = stylesheet.match(/\.lux-mobile-nav\s*\{\s*position:\s*fixed[^}]*\}/)?.[0] ?? "";

  assert.match(fixedMobileNavRule, /position:\s*fixed/);
  assert.match(fixedMobileNavRule, /top:\s*var\(--lux-header-height\)/);
});
