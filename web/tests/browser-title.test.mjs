import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

test("active web entry uses the built-in server title before the app starts", () => {
  const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
  assert.match(html, /<title>Lux Server - Lux<\/title>/);
});
