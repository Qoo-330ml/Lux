import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

test("detail poster and copy share a top alignment baseline", () => {
  const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");
  const detailGridRule = stylesheet.match(/\.lux-detail-grid\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(detailGridRule, /align-items:\s*start/);
});

test("active React pages do not render global back controls", () => {
  const sources = [
    "../src/features/detail/MediaDetailPage.tsx",
    "../src/features/player/PlayerPage.tsx",
    "../src/features/admin/AdminLayout.tsx",
  ].map((path) => readFileSync(new URL(path, import.meta.url), "utf8")).join("\n");

  assert.doesNotMatch(sources, /className="lux-detail-back"|lux-player-topbar[^\n]*返回媒体详情|className="lux-admin-back"/);
});
