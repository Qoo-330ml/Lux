import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

test("detail editors are split into lazy chunks and are not eagerly imported by the page", () => {
  const page = readFileSync(new URL("../src/features/detail/MediaDetailPage.tsx", import.meta.url), "utf8");
  const actions = readFileSync(new URL("../src/features/detail/MediaDetailActions.tsx", import.meta.url), "utf8");
  for (const editor of ["MediaMetadataEditor", "MediaImageEditor", "MediaSubtitleEditor", "MediaIdentifier", "MediaDeleteDialog"]) {
    assert.doesNotMatch(page, new RegExp(`import \\{?[^;]*${editor}[^;]* from \\"\\.\\./media/${editor}\\"`));
    assert.match(actions, new RegExp(`lazy\\(\\(\\) => import\\(\\"\\.\\./media/${editor}\\"\\)`));
  }
  assert.match(actions, /<Suspense fallback=\{null\}>/);
});
