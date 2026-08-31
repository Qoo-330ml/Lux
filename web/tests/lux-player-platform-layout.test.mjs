import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const styles = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");

test("LuxPlayer reserves dynamic viewport and safe-area space for mobile controls", () => {
  assert.match(styles, /\.lux-player-page \{[^}]*height: 100dvh/);
  assert.match(styles, /\.lux-player-frame \{[^}]*top: env\(safe-area-inset-top\)/);
  assert.match(styles, /\.lux-player-topbar \{[^}]*top: calc\(24px \+ env\(safe-area-inset-top\)\)/);
  assert.match(styles, /\.lux-player-controls-wrap \{[^}]*bottom: calc\(10px \+ env\(safe-area-inset-bottom\)\)/);
  assert.match(styles, /\.lux-player-settings-popover \{[^}]*right: calc\(2% \+ env\(safe-area-inset-right\)\)/);
});

test("LuxPlayer keeps danmaku out of the title and control safe zones", () => {
  assert.match(
    styles,
    /\.lux-player-danmaku-overlay \{[^}]*inset: clamp\(88px, 12vh, 120px\) 0 clamp\(92px, 18vh, 156px\) 0/,
  );
});

test("LuxPlayer mini progress stays at the edge without taking pointer focus", () => {
  assert.match(styles, /\.lux-player-mini-progress \{[^}]*bottom: calc\(4px \+ env\(safe-area-inset-bottom\)\)/);
  assert.match(styles, /\.lux-player-mini-progress \{[^}]*pointer-events: none/);
});

test("LuxPlayer chapter markers overlay the shared timeline instead of adding a second rail", () => {
  assert.match(styles, /\.lux-player-chapter-rail \{[^}]*position: absolute/);
  assert.match(styles, /\.lux-player-chapter-rail \{[^}]*pointer-events: none/);
});

test("LuxPlayer loading and error states center their content across the full viewport", () => {
  const stateRule = styles.match(/\.lux-player-page-loading, \.lux-player-page-error \{([^}]*)\}/)?.[1] ?? "";

  assert.match(stateRule, /display:\s*flex/);
  assert.match(stateRule, /flex-direction:\s*column/);
  assert.match(stateRule, /align-items:\s*center/);
  assert.match(stateRule, /justify-content:\s*center/);
});
