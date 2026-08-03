import { describe, expect, it } from "vitest";
import { heroSlides } from "../src/features/home/carousel";

const item = (id: string, itemType = "MOVIE") => ({ id, title: id, itemType });

describe("heroSlides", () => {
  it("keeps continue-watching first and appends unique recently added items", () => {
    expect(heroSlides({
      continueWatching: [item("continue-1"), item("shared")],
      recentlyAdded: [item("shared"), item("recent-1"), item("recent-2")],
    }).map((media) => media.id)).toEqual(["continue-1", "shared", "recent-1", "recent-2"]);
  });

  it("returns an empty slide list when home shelves are empty", () => {
    expect(heroSlides({}).length).toBe(0);
  });
});
