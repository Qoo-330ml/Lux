// @vitest-environment jsdom

import { describe, expect, it } from "vitest";
import { mediaTypeLabel } from "../src/features/home/media";

describe("mediaTypeLabel", () => {
  it("does not label episodes or seasons as movies", () => {
    expect(mediaTypeLabel("MOVIE")).toBe("电影");
    expect(mediaTypeLabel("SERIES")).toBe("剧集");
    expect(mediaTypeLabel("SEASON")).toBe("季度");
    expect(mediaTypeLabel("EPISODE")).toBe("单集");
    expect(mediaTypeLabel("BOX_SET")).toBe("合集");
  });
});
