import { readFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import assert from "node:assert/strict";

const viteConfigPath = resolve(dirname(fileURLToPath(import.meta.url)), "../vite.config.ts");
const viteConfigSource = await readFile(viteConfigPath, "utf8");

test("the dev server forwards WebSocket upgrades for the realtime endpoint", () => {
  // `pnpm --dir web dev` serves the app on 5173; the realtime client connects to
  // ws://127.0.0.1:5173/api/v1/ws, which the proxy must upgrade to LUX_API_ORIGIN.
  assert.match(viteConfigSource, /"\/api":\s*\{[^}]*ws:\s*true/);
});

test("the dev proxy leaves the Host header alone so the strict origin check passes", () => {
  // ADR-054 D5 compares the Origin host[:port] with the Host header. Rewriting Host to the
  // upstream address (changeOrigin) makes every handshake fail.
  assert.doesNotMatch(viteConfigSource, /changeOrigin:\s*true/);
  assert.match(viteConfigSource, /changeOrigin/, "the proxy block documents why changeOrigin stays off");
});
