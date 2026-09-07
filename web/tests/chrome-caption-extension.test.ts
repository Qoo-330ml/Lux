import { describe, expect, it } from "vitest";
import { isHttpUrl, validateChromeMediaRange } from "../src/features/player/chrome-caption-extension";

describe("Chrome caption extension bridge", () => {
  it("accepts only HTTP(S) media URLs", () => {
    expect(isHttpUrl("https://cdn.example.test/movie.mkv")).toBe(true);
    expect(isHttpUrl("http://127.0.0.1:8097/api/v1/playback/direct")).toBe(true);
    expect(isHttpUrl("/api/v1/playback/direct")).toBe(false);
    expect(isHttpUrl("file:///tmp/movie.mkv")).toBe(false);
    expect(isHttpUrl("javascript:alert(1)")).toBe(false);
  });

  it("accepts a complete range response and rejects a truncated response", () => {
    const data = new Uint8Array([1, 2, 3, 4]).buffer;
    expect(validateChromeMediaRange({
      data,
      start: 10,
      end: 13,
      total: 20,
      etag: '"v1"',
    })).toMatchObject({ start: 10, end: 13, total: 20, etag: '"v1"' });
    expect(validateChromeMediaRange({
      data: data.slice(0, 3),
      start: 10,
      end: 13,
      total: 20,
      etag: null,
    })).toBeNull();
  });

  it("rejects a range that claims an unknown total", () => {
    expect(validateChromeMediaRange({
      data: new Uint8Array([1]).buffer,
      start: 0,
      end: 0,
      total: 0,
      etag: null,
    })).toBeNull();
  });
});
