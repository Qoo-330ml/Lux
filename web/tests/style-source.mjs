import { readdirSync, readFileSync } from "node:fs";
import path from "node:path";

const sourceRoot = path.resolve(import.meta.dirname, "../src");
const baseStylesheet = path.join(sourceRoot, "react.css");

function walk(directory) {
  const files = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const full = path.join(directory, entry.name);
    if (entry.isDirectory()) files.push(...walk(full));
    else if (entry.name.endsWith(".css")) files.push(full);
  }
  return files;
}

function queryNeedle(query) {
  const trimmed = query.trim();
  return trimmed.startsWith("@media") ? trimmed : `@media ${trimmed}`;
}

// react.css is loaded by the app shell and route stylesheets after it, so keep
// the base file first and let route files win, matching the browser cascade.
const files = [baseStylesheet, ...walk(sourceRoot).sort().filter((file) => file !== baseStylesheet)];

// Tests assert source-level CSS contracts. Route CSS is split out of react.css,
// so expose every stylesheet as one searchable source.
export const stylesheet = files.map((file) => readFileSync(file, "utf8")).join("\n");

// Return each stylesheet from its first matching @media query to that file's end.
// This keeps the original single-file `slice(indexOf(...))` behaviour on every
// route stylesheet: breakpoint blocks cascade downward, while rules that live in
// another file no longer leak in ahead of the base rules.
export function mediaStyles(query) {
  const needle = queryNeedle(query);
  const chunks = [];
  for (const file of files) {
    const css = readFileSync(file, "utf8");
    const start = css.indexOf(needle);
    if (start !== -1) chunks.push(css.slice(start));
  }
  return chunks.join("\n");
}
