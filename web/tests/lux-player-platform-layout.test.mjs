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
