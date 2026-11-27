// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  clearHomeCarouselCache,
  heroSlides,
  heroTitleScale,
  readHomeCarouselCache,
  writeHomeCarouselCache,
} from "../src/features/home/carousel";

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

  it("limits the carousel to the first seven unique media items", () => {
    expect(heroSlides({
      continueWatching: [item("continue-1"), item("continue-2"), item("continue-3"), item("continue-4")],
      recentlyAdded: [item("recent-1"), item("recent-2"), item("recent-3"), item("recent-4"), item("recent-5")],
    }).map((media) => media.id)).toEqual([
      "continue-1",
      "continue-2",
      "continue-3",
      "continue-4",
      "recent-1",
      "recent-2",
      "recent-3",
    ]);
  });
});

describe("heroTitleScale", () => {
  it("keeps short titles at the default display size", () => {
    expect(heroTitleScale("精选电影")).toBe("default");
  });

  it("uses the compact display size for moderately long titles", () => {
    expect(heroTitleScale("这是一个包含很多中文字符的中长标题，用于测试轮播显示效果")).toBe("compact");
  });

  it("uses the smallest readable display size for very long mixed titles", () => {
    expect(heroTitleScale("FC2-4916281 脸）强忍着因为嘘息而即将失禁，但在猛烈的冲击下不停地溢出")).toBe("small");
  });

  it("treats an empty title as a short title", () => {
    expect(heroTitleScale("   ")).toBe("default");
  });
});

describe("homeCarouselCache", () => {
  afterEach(() => {
    vi.useRealTimers();
    clearHomeCarouselCache();
  });

  it("reuses cached recommendations within the same UTC 02:00 batch and TTL", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-11-27T01:55:00Z"));
    writeHomeCarouselCache("user-1", { recommended: [item("slide-1")] });

    vi.setSystemTime(new Date("2026-11-27T01:58:00Z"));
    expect(readHomeCarouselCache("user-1")?.data.recommended.map((entry) => entry.id)).toEqual(["slide-1"]);
  });

  it("invalidates cached recommendations immediately when crossing the UTC 02:00 batch boundary", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-11-27T01:59:50Z"));
    writeHomeCarouselCache("user-1", { recommended: [item("yesterday-slide")] });

    vi.setSystemTime(new Date("2026-11-27T02:00:05Z"));
    expect(readHomeCarouselCache("user-1")).toBeUndefined();
  });
});
