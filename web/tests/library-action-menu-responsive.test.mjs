import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");
const mobileRules = stylesheet.match(/@media \(max-width: 720px\) \{([\s\S]*?)\n\}/)?.[1] ?? "";
const menuRule = mobileRules.match(/\.lux-library-action-menu\s*\{([^}]*)\}/)?.[1] ?? "";

test("mobile library action menu remains inside the dynamic viewport", () => {
  assert.match(menuRule, /position:\s*fixed/);
  assert.match(menuRule, /bottom:\s*calc\(16px \+ env\(safe-area-inset-bottom\)\)/);
  assert.match(menuRule, /max-height:\s*calc\(100dvh - 32px - env\(safe-area-inset-bottom\)\)/);
  assert.match(menuRule, /overflow-y:\s*auto/);
  assert.match(menuRule, /overscroll-behavior:\s*contain/);
});

test("mobile library action menu keeps touch targets at least 44 pixels tall", () => {
  assert.match(mobileRules, /\.lux-library-action-menu button\s*\{[^}]*min-height:\s*var\(--lux-button-height-touch\)/);
});
