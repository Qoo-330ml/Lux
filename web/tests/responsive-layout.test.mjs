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
    ".lux-admin-layout",
  ]) {
    assert.match(rule(selector), responsiveWidth, selector);
  }
});

test("player overlays use viewport-relative safe-area insets", () => {
  const safeAreaInset = /(?:left|right):\s*calc\(4%\s*\+\s*env\(safe-area-inset-(?:left|right)\)\)/;

  assert.match(rule(".lux-player-topbar"), safeAreaInset);
  assert.match(rule(".lux-player-frame"), /left:\s*env\(safe-area-inset-left\)/);
  assert.match(rule(".lux-player-frame"), /right:\s*env\(safe-area-inset-right\)/);
});

test("portrait phone players move AirPlay and picture-in-picture to the top bar", () => {
  assert.match(stylesheet, /\.lux-player-topbar-actions\s*\{[^}]*display:\s*none/);
  assert.match(stylesheet, /@media\s*\(max-width:\s*720px\)\s+and\s+\(orientation:\s*portrait\)[\s\S]*?\.lux-player-topbar-actions\s*\{[^}]*display:\s*flex/);
  assert.match(stylesheet, /@media\s*\(max-width:\s*720px\)\s+and\s+\(orientation:\s*portrait\)[\s\S]*?\.lux-player-controls \.lux-player-mobile-top-control\s*\{[^}]*display:\s*none/);
});

test("center playback action uses a smaller visual footprint", () => {
  assert.match(stylesheet, /\.lux-player-center-play, \.lux-player-center-splash\s*\{[^}]*width:\s*88px;[^}]*height:\s*88px/);
  assert.match(stylesheet, /@media\s*\(max-width:\s*560px\)[\s\S]*?\.lux-player-center-play, \.lux-player-center-splash\s*\{[^}]*width:\s*68px;[^}]*height:\s*68px/);
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
