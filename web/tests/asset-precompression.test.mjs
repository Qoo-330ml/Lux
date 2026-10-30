import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import zlib from "node:zlib";

const assets = path.resolve(import.meta.dirname, "..", "dist", "assets");
const extensions = new Set([".js", ".css", ".svg", ".html", ".json"]);
// The precompression helper skips assets below this size; tower-http's precompressed_gzip
// falls back to the uncompressed file, so tiny chunks legitimately ship without a sibling.
const minimumBytes = 512;
const repositoryRoot = path.resolve(import.meta.dirname, "../..");
const webRoot = path.join(repositoryRoot, "web");

test("precompression build helper is inside Docker's copied Web directory", () => {
  const packageJson = JSON.parse(
    fs.readFileSync(path.join(webRoot, "package.json"), "utf8"),
  );
  const invocation = packageJson.scripts.build.match(/(?:^|&&)\s*node\s+([^\s]+)/);
  assert.ok(invocation, "Web build must invoke the precompression helper with Node");

  const helperPath = path.resolve(webRoot, invocation[1]);
  const relativeHelperPath = path.relative(webRoot, helperPath);
  assert.ok(
    relativeHelperPath !== ".." && !relativeHelperPath.startsWith(`..${path.sep}`),
    `precompression helper must be inside Docker's copied web directory: ${invocation[1]}`,
  );
  assert.ok(fs.statSync(helperPath).isFile(), "precompression helper must exist");

  const dockerfile = fs.readFileSync(path.join(repositoryRoot, "Dockerfile"), "utf8");
  const webBuilderStage = dockerfile
    .split(/(?=^FROM )/m)
    .find((stage) => /^FROM .* AS web-builder$/m.test(stage));
  assert.ok(webBuilderStage, "Dockerfile must define the web-builder stage");
  assert.match(webBuilderStage, /^WORKDIR \/src\/web$/m);
  const copyWebIndex = webBuilderStage.indexOf("COPY web ./");
  const buildIndex = webBuilderStage.indexOf("RUN pnpm build");
  assert.ok(
    copyWebIndex >= 0 && buildIndex > copyWebIndex,
    "Docker web-builder must copy the Web sources before running the build",
  );
});

test("built Web assets have matching gzip siblings", () => {
  assert.ok(fs.existsSync(assets), "run pnpm --dir web build before this test");
  const files = fs.readdirSync(assets).filter((name) => extensions.has(path.extname(name)));
  assert.ok(files.length > 0, "the build produced no static assets");
  for (const name of files) {
    const source = path.join(assets, name);
    if (fs.statSync(source).size < minimumBytes) continue;
    const compressed = `${source}.gz`;
    assert.ok(fs.existsSync(compressed), `missing ${name}.gz`);
    assert.deepEqual(
      zlib.gunzipSync(fs.readFileSync(compressed)),
      fs.readFileSync(source),
      `${name}.gz does not decompress to the source asset`,
    );
  }
});
