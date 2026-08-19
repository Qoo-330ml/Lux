import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const here = dirname(fileURLToPath(import.meta.url));
const styles = readFileSync(resolve(here, "../src/react.css"), "utf8");

test("admin operations heading restores visible title copy without changing other admin headers", () => {
  const titleRule = styles.match(/\.lux-admin-page-heading\s*>\s*div\s*\{([^}]*)\}/)?.[1] ?? "";
  const headingRule = styles.match(/\.lux-admin-page-heading\s*\{([^}]*)\}/)?.[1] ?? "";
  const emptyHeadingRule = styles.match(/\.lux-admin-page-heading:not\(:has\(> \.lux-button\)\)\s*\{([^}]*)\}/)?.[1] ?? "";
  const operationsTitleRule = styles.match(/\.lux-operations-heading\s*>\s*div\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(titleRule, /position:\s*absolute/);
  assert.match(titleRule, /width:\s*1px/);
  assert.match(titleRule, /clip:\s*rect\(0 0 0 0\)/);
  assert.match(headingRule, /justify-content:\s*flex-end/);
  assert.match(emptyHeadingRule, /height:\s*0/);
  assert.match(emptyHeadingRule, /margin-bottom:\s*0/);
  assert.match(operationsTitleRule, /position:\s*static/);
  assert.match(operationsTitleRule, /width:\s*auto/);
  assert.match(operationsTitleRule, /overflow:\s*visible/);
  assert.match(operationsTitleRule, /clip:\s*auto/);
});
