import { describe, expect, it, vi } from "vitest";
import {
  clearLibraryFilterParams,
  isLibraryFilterYear,
  libraryFilterQueryKey,
  libraryFilterQueryOptions,
  libraryFiltersActive,
  libraryTypeFilterValues,
  readLibraryFilters,
  setLibraryFilterParam,
} from "../src/features/library/libraryFilters";
import { LuxApiClient } from "../src/lib/api/client";

describe("library filter helpers", () => {
  it("reads supported filters from the query string", () => {
    const filters = readLibraryFilters(
      new URLSearchParams("type=series&year=2024&status=unplayed&favorite=1"),
      "MIXED",
    );

    expect(filters).toEqual({ itemType: "SERIES", year: 2024, played: false, favoriteOnly: true });
  });

  it("drops a type filter the library cannot serve", () => {
    const filters = readLibraryFilters(new URLSearchParams("type=SERIES"), "MOVIE");

    expect(filters.itemType).toBeUndefined();
  });

  it("keeps a structurally valid type before the library record arrives", () => {
    expect(readLibraryFilters(new URLSearchParams("type=SERIES")).itemType).toBe("SERIES");
  });

  it("ignores malformed years and statuses instead of forwarding them", () => {
    const filters = readLibraryFilters(
      new URLSearchParams("year=abcd&status=maybe&favorite=0&type=VIDEO"),
      "MIXED",
    );

    expect(filters).toEqual({ favoriteOnly: false });
    expect(isLibraryFilterYear("2024")).toBe(true);
    expect(isLibraryFilterYear("24")).toBe(false);
    expect(isLibraryFilterYear("1200")).toBe(false);
  });

  it("only offers a type filter for libraries that mix movies and series", () => {
    expect(libraryTypeFilterValues("MIXED")).toEqual(["MOVIE", "SERIES"]);
    expect(libraryTypeFilterValues("MOVIE")).toEqual(["MOVIE"]);
    expect(libraryTypeFilterValues("SERIES")).toEqual(["SERIES"]);
    expect(libraryTypeFilterValues("HOMEVIDEOS")).toEqual([]);
    expect(libraryTypeFilterValues(undefined)).toEqual([]);
  });

  it("only forwards filters that are active", () => {
    expect(libraryFilterQueryOptions({ favoriteOnly: false })).toEqual({});
    expect(libraryFilterQueryOptions({ itemType: "MOVIE", year: 2024, played: true, favoriteOnly: true })).toEqual({
      year: 2024,
      isPlayed: true,
      isFavorite: true,
    });
    expect(libraryFilterQueryOptions({ played: false })).toEqual({ isPlayed: false });
  });

  it("separates cached pages per filter combination", () => {
    expect(libraryFilterQueryKey({ favoriteOnly: false })).toBe("none");
    expect(libraryFilterQueryKey({ year: 2024 })).not.toBe("none");
    expect(libraryFilterQueryKey({ itemType: "MOVIE" })).not.toBe(libraryFilterQueryKey({ itemType: "SERIES" }));
    expect(libraryFilterQueryKey({ played: false })).not.toBe(libraryFilterQueryKey({ played: true }));
  });

  it("reports whether any filter is active", () => {
    expect(libraryFiltersActive({ favoriteOnly: false })).toBe(false);
    expect(libraryFiltersActive({ favoriteOnly: true })).toBe(true);
    expect(libraryFiltersActive({ played: false })).toBe(true);
    expect(libraryFiltersActive({ year: 2024 })).toBe(true);
  });

  it("writes and clears filter parameters without touching other query parameters", () => {
    const base = new URLSearchParams("folderPath=abc&year=2024&status=played");

    expect(setLibraryFilterParam(base, "type", "MOVIE").toString()).toBe("folderPath=abc&year=2024&status=played&type=MOVIE");
    expect(setLibraryFilterParam(base, "year", undefined).toString()).toBe("folderPath=abc&status=played");
    expect(clearLibraryFilterParams(base).toString()).toBe("folderPath=abc");
  });
});

describe("library item requests", () => {
  it("sends year, played and favorite filters with their backend parameter names", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ items: [], total: 0 }), { status: 200 }),
    );

    await new LuxApiClient().libraryItems("movie-library", 2, "MOVIE", {
      sortBy: "Name",
      sortOrder: "Ascending",
      year: 2024,
      isPlayed: false,
      isFavorite: true,
    });

    expect(fetchMock.mock.calls[0]?.[0]).toBe(
      "/api/v1/libraries/movie-library/items?page=2&pageSize=24&itemType=MOVIE&sortBy=Name&sortOrder=Ascending&year=2024&is_played=false&is_favorite=true",
    );
  });

  it("keeps the unfiltered request unchanged", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ items: [], total: 0 }), { status: 200 }),
    );

    await new LuxApiClient().libraryItems("movie-library", 1, "MOVIE,SERIES", {
      sortBy: "Name",
      sortOrder: "Ascending",
      fields: "card",
      count: false,
    });

    expect(fetchMock.mock.calls[0]?.[0]).toBe(
      "/api/v1/libraries/movie-library/items?page=1&pageSize=24&itemType=MOVIE%2CSERIES&sortBy=Name&sortOrder=Ascending&fields=card&count=false",
    );
  });
});
