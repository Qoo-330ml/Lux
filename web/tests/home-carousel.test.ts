import { describe, expect, it } from "vitest";
import { heroSlides } from "../src/features/home/carousel";

const item = (id: string, itemType = "MOVIE") => ({ id, title: id, itemType });

describe("heroSlides", () => {
  it("uses server-ranked recommendations before legacy home shelves", () => {
    expect(heroSlides({
      recommended: [item("recommended-1"), item("shared")],
      continueWatching: [item("continue-1"), item("shared")],
      recentlyAdded: [item("recent-1")],
    }).map((media) => media.id)).toEqual([
      "recommended-1",
      "shared",
      "continue-1",
      "recent-1",
    ]);
  });

  it("keeps continue-watching first and appends unique recently added items", () => {
    expect(heroSlides({
      continueWatching: [item("continue-1"), item("shared")],
      recentlyAdded: [item("shared"), item("recent-1"), item("recent-2")],
    }).map((media) => media.id)).toEqual(["continue-1", "shared", "recent-1", "recent-2"]);
  });

  it("returns an empty slide list when home shelves are empty", () => {
    expect(heroSlides({}).length).toBe(0);
  });

  it("limits the carousel to the first five unique media items", () => {
    expect(heroSlides({
      continueWatching: [item("continue-1"), item("continue-2"), item("continue-3")],
      recentlyAdded: [item("recent-1"), item("recent-2"), item("recent-3")],
    }).map((media) => media.id)).toEqual([
      "continue-1",
      "continue-2",
      "continue-3",
      "recent-1",
      "recent-2",
    ]);
  });
});
