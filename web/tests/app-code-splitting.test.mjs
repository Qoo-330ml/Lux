import { readFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import assert from "node:assert/strict";

const appPath = resolve(dirname(fileURLToPath(import.meta.url)), "../src/app/App.tsx");
const appSource = await readFile(appPath, "utf8");
const viteConfigPath = resolve(dirname(fileURLToPath(import.meta.url)), "../vite.config.ts");
const viteConfigSource = await readFile(viteConfigPath, "utf8");

test("authenticated routes defer non-home page bundles", () => {
  assert.match(appSource, /lazy\(\(\) => import\("\.\.\/features\/library\/LibraryPage"\)\.then/);
  assert.match(appSource, /lazy\(\(\) => import\("\.\.\/features\/detail\/MediaDetailPage"\)\.then/);
  assert.match(appSource, /lazy\(\(\) => import\("\.\.\/features\/admin\/AdminLayout"\)\.then/);
  assert.doesNotMatch(appSource, /import \{ AdminDashboardPage \} from/);
  assert.doesNotMatch(appSource, /import \{ MediaDetailPage \} from/);
  assert.doesNotMatch(appSource, /path="emby-migration"/);
});

test("production JavaScript and assets use content-hashed filenames", () => {
  assert.match(viteConfigSource, /entryFileNames:\s*"assets\/\[name\]-\[hash\]\.js"/);
  assert.match(viteConfigSource, /chunkFileNames:\s*"assets\/\[name\]-\[hash\]\.js"/);
  assert.match(viteConfigSource, /assetFileNames:\s*"assets\/\[name\]-\[hash\]\[extname\]"/);
});

test("failed lazy imports show a reload path instead of leaving a blank page", () => {
  assert.match(appSource, /class RouteErrorBoundary extends Component/);
  assert.match(appSource, /<RouteErrorBoundary>\s*<Suspense/s);
  assert.match(appSource, /页面加载失败/);
  assert.match(appSource, /window\.location\.reload\(\)/);
});
