import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";

const webSourceDirectory = new URL("../src/", import.meta.url);

function sourceFiles(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) return sourceFiles(path);
    return /\.(mjs|tsx)$/.test(entry.name) ? [path] : [];
  });
}

test("user-facing eyebrows do not contain English headings", () => {
  const files = sourceFiles(webSourceDirectory.pathname);
  const reactEyebrow = /<span className="lux-eyebrow">[^<{]*[A-Za-z][^<{]*<\/span>|eyebrow="[^"]*[A-Za-z][^"]*"/;
  const legacyEyebrow = /<span class="eyebrow">[A-Za-z][^<]*<\/span>/;
  const legacyHeading = /<(?:h[1-6]|span class="eyebrow"|small)[^>]*>[^<]*(?:Next Up|Pending Queue|Personal Media|Personal Collection|Server Health|Lux Media Center|Initialization)[^<]*</;

  for (const file of files) {
    const source = readFileSync(file, "utf8");
    assert.doesNotMatch(source, reactEyebrow, `${file} contains an English React eyebrow`);
    assert.doesNotMatch(source, legacyEyebrow, `${file} contains an English legacy eyebrow`);
    assert.doesNotMatch(source, legacyHeading, `${file} contains an English legacy heading`);
  }
});
