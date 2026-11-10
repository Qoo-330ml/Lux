import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { parseDanmakuXml } from "../src/features/player/danmaku";

/**
 * The server (`src/application/danmaku/xml.rs`) and the Web parser must agree on what a usable
 * danmaku entry is. Both read this one fixture, so tightening one side without the other fails a
 * test instead of silently diverging (`docs/LUX-DEVELOPMENT.md` §14.5).
 */
type DanmakuValidityVector = {
  name: string;
  note?: string;
  xml: string;
  valid: number;
  dropped: number;
};

const vectors: DanmakuValidityVector[] = JSON.parse(
  readFileSync(
    fileURLToPath(new URL("../../tests/fixtures/danmaku/validity-vectors.json", import.meta.url)),
    "utf8",
  ),
).vectors;

describe("shared danmaku validity vectors", () => {
  it("loads the vectors the Rust inspector also asserts", () => {
    expect(vectors.length).toBeGreaterThan(10);
  });

  for (const vector of vectors) {
    it(`${vector.name} matches the Rust entry counts`, () => {
      const result = parseDanmakuXml(vector.xml);
      const entryCount = (vector.xml.match(/<d\b/gi) ?? []).length;
      expect(result.totalEntries).toBe(vector.valid);
      expect(entryCount).toBe(vector.valid + vector.dropped);
      expect(result.entries).toHaveLength(vector.valid);
    });
  }
});
