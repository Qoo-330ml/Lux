import test from "node:test";
import assert from "node:assert/strict";
import {
  clearCsrfToken,
  readCsrfToken,
  rememberCsrfToken,
  requestOptions,
} from "../src/request-options.mjs";

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

test("legacy web requests use the stored CSRF token when the cookie is missing", () => {
  const previousStorage = globalThis.localStorage;
  let storedValue = null;
  globalThis.localStorage = {
    getItem(key) {
      return key === "lux_csrf_token" ? storedValue : null;
    },
    setItem(key, value) {
      if (key === "lux_csrf_token") storedValue = value;
    },
    removeItem(key) {
      if (key === "lux_csrf_token") storedValue = null;
    },
  };
  try {
    rememberCsrfToken("csrf-from-login");

    assert.equal(readCsrfToken(), "csrf-from-login");
    assert.equal(requestOptions({ method: "POST" }).headers["x-csrf-token"], "csrf-from-login");

    clearCsrfToken();
    assert.equal(readCsrfToken(), "");
  } finally {
    if (previousStorage === undefined) {
      delete globalThis.localStorage;
    } else {
      globalThis.localStorage = previousStorage;
    }
  }
});

test("legacy web requests keep the login CSRF nonce in memory when privacy mode blocks storage", () => {
  const previousDocument = globalThis.document;
  const previousStorage = globalThis.localStorage;
  globalThis.document = {
    get cookie() {
      return "";
    },
    set cookie(_value) {
      throw new Error("client cookies blocked");
    },
  };
  globalThis.localStorage = {
    getItem() {
      throw new Error("localStorage blocked");
    },
    setItem() {
      throw new Error("localStorage blocked");
    },
    removeItem() {
      throw new Error("localStorage blocked");
    },
  };
  try {
    rememberCsrfToken("csrf-from-login");

    assert.equal(readCsrfToken(), "csrf-from-login");
    assert.equal(requestOptions({ method: "POST" }).headers["x-csrf-token"], "csrf-from-login");

    clearCsrfToken();
    assert.equal(readCsrfToken(), "");
  } finally {
    if (previousDocument === undefined) {
      delete globalThis.document;
    } else {
      globalThis.document = previousDocument;
    }
    if (previousStorage === undefined) {
      delete globalThis.localStorage;
    } else {
      globalThis.localStorage = previousStorage;
    }
  }
});
