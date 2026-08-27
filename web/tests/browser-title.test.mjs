import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

test("web entry documents use the built-in server title before the app starts", () => {
  for (const file of ["../index.html", "../src/index.html"]) {
    const html = readFileSync(new URL(file, import.meta.url), "utf8");
    assert.match(html, /<title>Lux Server - Lux<\/title>/, file);
  }
});
