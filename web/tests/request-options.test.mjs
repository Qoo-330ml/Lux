import test from "node:test";
import assert from "node:assert/strict";
import { requestOptions } from "../src/request-options.mjs";

test("request options preserve CSRF and JSON headers together", () => {
  const options = requestOptions({
    method: "POST",
    headers: { "x-csrf-token": "test-csrf" },
    body: "{}",
  });

  assert.equal(options.credentials, "same-origin");
  assert.equal(options.headers.Accept, "application/json");
  assert.equal(options.headers["x-csrf-token"], "test-csrf");
  assert.equal(options.headers["Content-Type"], "application/json");
});
