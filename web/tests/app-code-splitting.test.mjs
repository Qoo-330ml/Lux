import { readFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import assert from "node:assert/strict";

const appPath = resolve(dirname(fileURLToPath(import.meta.url)), "../src/app/App.tsx");
const appSource = await readFile(appPath, "utf8");
const adminLayoutPath = resolve(dirname(fileURLToPath(import.meta.url)), "../src/features/admin/AdminLayout.tsx");
const adminLayoutSource = await readFile(adminLayoutPath, "utf8");
const stylesPath = resolve(dirname(fileURLToPath(import.meta.url)), "../src/react.css");
const stylesSource = await readFile(stylesPath, "utf8");
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

test("lazy user routes use a page fallback inside the persistent shell", () => {
  assert.match(appSource, /<Route path="watch\/\:itemId" element={<PlayerPage \/>} \/>/);
  assert.match(appSource, /<Route path="libraries" element={<SuspensePage><LibrariesPage \/><\/SuspensePage>} \/>/);
  assert.match(appSource, /<Route path="libraries\/\:libraryId" element={<SuspensePage><LibraryPage serverName=\{serverName\} \/><\/SuspensePage>} \/>/);
  assert.match(appSource, /<Route path="favorites" element={<SuspensePage><FavoritesPage \/><\/SuspensePage>} \/>/);
  assert.match(appSource, /<Route path="items\/\:itemId" element={<SuspensePage><MediaDetailPage \/><\/SuspensePage>} \/>/);
  assert.match(appSource, /<Route path="collections\/\:collectionId" element={<SuspensePage><CollectionDetailPage \/><\/SuspensePage>} \/>/);
  assert.match(appSource, /<Route path="people\/\:personId" element={<SuspensePage><PersonDetailPage user=\{user\} \/><\/SuspensePage>} \/>/);
  assert.match(appSource, /<Route path="search" element={<SuspensePage><SearchPage \/><\/SuspensePage>} \/>/);
  assert.match(appSource, /<Route path="account\/\:userId" element={<SuspensePage><AccountRoute user=\{user\} \/><\/SuspensePage>} \/>/);
  assert.match(appSource, /<Route path="admin" element=\{user\.canManageServer\s*\?\s*<SuspensePage><AdminLayout \/><\/SuspensePage>\s*:\s*<Navigate to="\/" replace \/>\s*\}>/);
  assert.match(appSource, /function SuspensePage\(\{ children \}: \{ children: ReactNode \}\)[\s\S]*?<Suspense fallback=\{<PageSkeleton \/>\}>/);
  assert.match(appSource, /role="status" aria-busy="true" aria-label="正在加载页面"/);
  assert.match(stylesSource, /\.lux-route-skeleton\s*\{/);
});

test("admin child pages suspend inside the admin content area", () => {
  assert.match(adminLayoutSource, /<div className="lux-admin-content">\s*<Suspense fallback=\{<AdminPageSkeleton \/>\}>\s*<Outlet \/>/);
  assert.match(adminLayoutSource, /role="status" aria-busy="true" aria-label="正在加载管理页面"/);
  assert.match(stylesSource, /\.lux-admin-page-skeleton\s*\{/);
});
