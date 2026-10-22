import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { mediaStyles, stylesheet as allStyles } from "./style-source.mjs";

const identifierStyles = readFileSync(new URL("../src/features/media/MediaIdentifier.css", import.meta.url), "utf8");
const globalStyles = allStyles;

test("metadata identifier children cannot widen the mobile dialog", () => {
  assert.match(identifierStyles, /\.lux-identifier-search\s*\{[^}]*min-width:\s*0[^}]*width:\s*100%[^}]*overflow-x:\s*hidden/s);
  assert.match(identifierStyles, /\.lux-identifier-search input\s*\{[^}]*min-width:\s*0[^}]*width:\s*100%/s);
  assert.match(identifierStyles, /\.lux-identifier-results\s*\{[^}]*min-width:\s*0[^}]*width:\s*100%[^}]*overflow-x:\s*hidden/s);
});

test("mobile media dialogs use a viewport-bound width and touch-sized controls", () => {
  const mobileRules = mediaStyles("@media (max-width: 560px)");
  assert.match(mobileRules, /\.lux-media-editor\s*\{[^}]*width:\s*calc\(100dvw - 24px\)/s);
  assert.match(mobileRules, /\.lux-media-editor-close\s*\{[^}]*width:\s*44px;\s*height:\s*44px/s);
  assert.match(identifierStyles, /@media \(max-width: 560px\)[\s\S]*\.lux-identifier-search\s*\{[^}]*grid-template-columns:\s*minmax\(0, 1fr\)/);
});
