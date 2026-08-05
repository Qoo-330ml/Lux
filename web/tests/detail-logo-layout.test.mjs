import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

test("detail logo sits above the title in the copy column", () => {
  const source = readFileSync(new URL("../src/features/detail/MediaDetailPage.tsx", import.meta.url), "utf8");
  const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");
  const titleRowStart = source.indexOf('className="lux-detail-title-row"');
  const titleRowEnd = source.indexOf("</div>", titleRowStart);
  const titleRow = titleRowStart >= 0 && titleRowEnd > titleRowStart ? source.slice(titleRowStart, titleRowEnd) : "";
  const titleRowRule = stylesheet.match(/\.lux-detail-title-row\s*\{([^}]*)\}/)?.[1] ?? "";
  const logoRule = stylesheet.match(/\.lux-detail-logo\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.ok(titleRow.includes('className="lux-detail-logo"'), "the logo should be rendered in the title row");
  assert.ok(titleRow.indexOf('className="lux-detail-logo"') < titleRow.indexOf("<h1>"), "the logo should appear before the title");
  assert.match(titleRowRule, /flex-direction:\s*column/);
  assert.match(titleRowRule, /align-items:\s*flex-start/);
  assert.match(logoRule, /max-width:\s*min\(360px,\s*100%\)/);
  assert.match(logoRule, /max-height:\s*96px/);
});
