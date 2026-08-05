import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");
const mediaSource = readFileSync(new URL("../src/features/home/media.tsx", import.meta.url), "utf8");

test("home media actions are hidden until the poster is hovered and sit in its upper-right corner", () => {
  const actionsRule = stylesheet.match(/\.lux-media-art-shell\s*>\s*\.lux-media-actions\s*\{([^}]*)\}/)?.[1] ?? "";
  const visibleRule = stylesheet.match(/\.lux-media-art-shell:hover\s*>\s*\.lux-media-actions,\s*\.lux-media-art-shell:focus-within\s*>\s*\.lux-media-actions\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(mediaSource, /className="lux-media-art-shell"/);
  assert.match(actionsRule, /top:\s*9px/);
  assert.match(actionsRule, /bottom:\s*auto/);
  assert.match(actionsRule, /opacity:\s*0/);
  assert.match(actionsRule, /pointer-events:\s*none/);
  assert.match(visibleRule, /opacity:\s*1/);
  assert.match(visibleRule, /pointer-events:\s*auto/);
});
