import { describe, expect, it } from "vitest";
import { libraryItemTypeFilter } from "../src/features/library/LibraryPage";

describe("libraryItemTypeFilter", () => {
  it("shows only series at the root of a series library", () => {
    expect(libraryItemTypeFilter("SERIES")).toBe("SERIES");
  });

  it("shows only movies at the root of a movie library", () => {
    expect(libraryItemTypeFilter("MOVIE")).toBe("MOVIE");
  });

  it("shows both top-level types in a mixed library", () => {
    expect(libraryItemTypeFilter("MIXED")).toBe("MOVIE,SERIES");
  });
});
