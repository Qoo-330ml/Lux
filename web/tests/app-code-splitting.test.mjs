import { readFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import assert from "node:assert/strict";

const appPath = resolve(dirname(fileURLToPath(import.meta.url)), "../src/app/App.tsx");
const appSource = await readFile(appPath, "utf8");

test("authenticated routes defer non-home page bundles", () => {
  assert.match(appSource, /lazy\(\(\) => import\("\.\.\/features\/library\/LibraryPage"\)\.then/);
  assert.match(appSource, /lazy\(\(\) => import\("\.\.\/features\/detail\/MediaDetailPage"\)\.then/);
  assert.match(appSource, /lazy\(\(\) => import\("\.\.\/features\/admin\/AdminLayout"\)\.then/);
  assert.doesNotMatch(appSource, /import \{ AdminDashboardPage \} from/);
  assert.doesNotMatch(appSource, /import \{ MediaDetailPage \} from/);
  assert.doesNotMatch(appSource, /path="emby-migration"/);
});
