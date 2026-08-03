import { describe, expect, it } from "vitest";
import { formatAdminDate } from "../src/features/admin/date";

describe("formatAdminDate", () => {
  it("interprets numeric API timestamps as Unix seconds", () => {
    expect(formatAdminDate(1_735_689_600)).toContain("2025");
  });
});
