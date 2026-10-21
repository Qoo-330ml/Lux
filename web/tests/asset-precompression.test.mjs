import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import zlib from "node:zlib";

const assets = path.resolve(import.meta.dirname, "..", "dist", "assets");
const extensions = new Set([".js", ".css", ".svg", ".html", ".json"]);

test("built Web assets have matching gzip siblings", () => {
  assert.ok(fs.existsSync(assets), "run pnpm --dir web build before this test");
  const files = fs.readdirSync(assets).filter((name) => extensions.has(path.extname(name)));
  assert.ok(files.length > 0, "the build produced no static assets");
  for (const name of files) {
    const source = path.join(assets, name);
    const compressed = `${source}.gz`;
    assert.ok(fs.existsSync(compressed), `missing ${name}.gz`);
    assert.deepEqual(
      zlib.gunzipSync(fs.readFileSync(compressed)),
      fs.readFileSync(source),
      `${name}.gz does not decompress to the source asset`,
    );
  }
});
