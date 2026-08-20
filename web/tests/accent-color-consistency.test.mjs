import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

const stylesheetPaths = [
  "../src/react.css",
  "../src/features/admin/plugin-library.css",
  "../src/features/media/MediaSubtitleEditor.css",
];

const stylesheets = stylesheetPaths
  .map((path) => fs.readFileSync(new URL(path, import.meta.url), "utf8"))
  .join("\n");

function withoutThemeDefinitions(styles) {
  return styles
    .replace(/--lux-accent:\s*#d247bf;/g, "")
    .replace(/\.lux-accent-option\.is-berry[^\{]*\{[^}]*\}/g, "")
    .replace(/html\[data-lux-accent="berry"\][^\{]*\{[^}]*\}/g, "");
}

test("brand-colored UI follows the selected accent token", () => {
  const themedStyles = withoutThemeDefinitions(stylesheets);

  assert.match(stylesheets, /--lux-accent/);
  assert.doesNotMatch(
    themedStyles,
    /#d247bf|rgba\(210\s*,\s*71\s*,\s*191|#d783ca|#b8b5ff|rgba\(142\s*,\s*135\s*,\s*255|#f5a7e5|#f3a9e4|#efb2e4|#a73194|#8f2e80|#75a5d1|#769ac0|#70c96d|#4fb64b|#5bc456|#df5aca|rgba\(167\s*,\s*49\s*,\s*148|rgba\(164\s*,\s*45\s*,\s*145/,
  );
});
