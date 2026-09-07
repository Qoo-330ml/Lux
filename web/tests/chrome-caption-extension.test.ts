import { describe, expect, it } from "vitest";
import { isHttpUrl } from "../src/features/player/chrome-caption-extension";

describe("Chrome caption extension bridge", () => {
  it("accepts only HTTP(S) media URLs", () => {
    expect(isHttpUrl("https://cdn.example.test/movie.mkv")).toBe(true);
    expect(isHttpUrl("http://127.0.0.1:8097/api/v1/playback/direct")).toBe(true);
    expect(isHttpUrl("/api/v1/playback/direct")).toBe(false);
    expect(isHttpUrl("file:///tmp/movie.mkv")).toBe(false);
    expect(isHttpUrl("javascript:alert(1)")).toBe(false);
  });
});
