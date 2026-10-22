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

function withoutMediaBlocks(css) {
  let text = "";
  let index = 0;
  while (index < css.length) {
    const open = css.indexOf("@media", index);
    if (open === -1) {
      text += css.slice(index);
      break;
    }
    text += css.slice(index, open);
    const brace = css.indexOf("{", open);
    if (brace === -1) break;
    let depth = 0;
    let cursor = brace;
    while (cursor < css.length) {
      if (css[cursor] === "{") depth += 1;
      else if (css[cursor] === "}") {
        depth -= 1;
        if (depth === 0) break;
      }
      cursor += 1;
    }
    index = cursor + 1;
  }
  return text;
}

// Return the declarations of the last rule whose selector list contains
// `selector` verbatim, reading react.css first and route stylesheets after.
// The caller may pass a single selector or a whole comma-separated list; route
// sheets now own rules that react.css used to hold, so a plain first match would
// return a breakpoint override that still lives in react.css.
export function baseRule(selector) {
  const wanted = selector.replace(/\s+/g, " ").trim();
  let found = "";

  for (const file of files) {
    const text = withoutMediaBlocks(readFileSync(file, "utf8"));
    const matcher = /([^{}]+)\{([^{}]*)\}/g;
    let match = matcher.exec(text);
    while (match !== null) {
      const list = match[1].replace(/\s+/g, " ").trim();
      const components = list.split(",").map((part) => part.trim());
      if (list === wanted || components.includes(wanted)) found = match[2];
      match = matcher.exec(text);
    }
  }

  return found;
}
