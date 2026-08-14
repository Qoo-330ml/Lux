import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const styles = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");

test("primary page headings stay within the compact type scale", () => {
  assert.match(styles, /\.lux-hero-title\s*\{[^}]*font-size: clamp\(2\.75rem, 5\.5vw, 5rem\)/);
  assert.match(styles, /\.lux-page-heading h1 \{[^}]*font-size: clamp\(1\.75rem, 3vw, 2\.6rem\)/);
  assert.match(styles, /\.lux-admin-page-heading h1 \{[^}]*font-size: clamp\(1\.75rem, 3vw, 2\.6rem\)/);
  assert.match(styles, /\.lux-account-page-heading h1 \{[^}]*font-size: clamp\(1\.75rem, 3vw, 2\.6rem\)/);
  assert.match(styles, /\.lux-auth-card h1 \{[^}]*font-size: 1\.8rem/);
  assert.match(styles, /\.lux-detail-title-row h1 \{[^}]*font-size: 26px/);
});

test("secondary headings do not grow beyond the page title hierarchy", () => {
  assert.match(styles, /\.lux-library-strategy-heading h2 \{[^}]*font-size: clamp\(1\.4rem, 2\.5vw, 1\.9rem\)/);
  assert.match(styles, /\.lux-admin-server-copy h2 \{[^}]*font-size: clamp\(1\.2rem, 2vw, 1\.55rem\)/);
  assert.match(styles, /\.lux-admin-monitor-heading h2 \{[^}]*font-size: clamp\(1\.2rem, 2vw, 1\.55rem\)/);
});
