import { describe, expect, it } from "vitest";
import { pluginCategoryLabel } from "../src/features/admin/AdminPluginsPage";

describe("pluginCategoryLabel", () => {
  it("labels scraper plugins for administrators", () => {
    expect(pluginCategoryLabel("SCRAPER")).toBe("刮削器");
  });

  it("keeps unknown third-party categories visible", () => {
    expect(pluginCategoryLabel("TRANSCODER")).toBe("TRANSCODER");
  });
});
