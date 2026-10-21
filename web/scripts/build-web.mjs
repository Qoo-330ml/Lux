#!/usr/bin/env node
// Precompress the built Web assets so luxd can serve `.gz` files directly.
// Run automatically after `pnpm --dir web build` and from Docker's web build context.

import fs from "node:fs";
import path from "node:path";
import zlib from "node:zlib";

const root = path.resolve(import.meta.dirname, "..", "dist");
const extensions = new Set([".js", ".css", ".svg", ".html", ".json"]);
const minimumBytes = 512;

function walk(directory) {
  const entries = [];
  for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
    const fullPath = path.join(directory, entry.name);
    if (entry.isDirectory()) entries.push(...walk(fullPath));
    else entries.push(fullPath);
  }
  return entries;
}

if (!fs.existsSync(root)) {
  throw new Error(`web/dist is missing; run the Vite build first: ${root}`);
}

let generated = 0;
let skipped = 0;
let sourceBytes = 0;
let compressedBytes = 0;
for (const source of walk(root)) {
  if (!extensions.has(path.extname(source).toLowerCase())) continue;
  const stats = fs.statSync(source);
  if (stats.size < minimumBytes) continue;
  const target = `${source}.gz`;
  if (fs.existsSync(target) && fs.statSync(target).mtimeMs >= stats.mtimeMs) {
    skipped += 1;
    continue;
  }
  const content = fs.readFileSync(source);
  const compressed = zlib.gzipSync(content, { level: 9 });
  fs.writeFileSync(target, compressed);
  generated += 1;
  sourceBytes += content.length;
  compressedBytes += compressed.length;
}

const summary = {
  root,
  generated,
  skipped,
  sourceBytes,
  compressedBytes,
  ratio: sourceBytes === 0 ? null : compressedBytes / sourceBytes,
};
process.stdout.write(`${JSON.stringify(summary, null, 2)}\n`);
