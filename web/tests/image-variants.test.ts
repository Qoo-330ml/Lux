import { describe, expect, it } from "vitest";
import { imageUrl } from "../src/features/home/media-display";
import { imageVariantWidth, withImageWidth } from "../src/features/home/image-variants";
import type { MediaItem } from "../src/lib/api/types";

const item = {
  id: "item-1",
  itemType: "MOVIE",
  title: "测试影片",
  imageTags: { poster: "poster-tag", fanart: "fanart-tag" },
} as MediaItem;

describe("image variant URLs", () => {
  it("keeps the existing URL byte-for-byte when no width is requested", () => {
    expect(imageUrl(item)).toBe("/api/v1/items/item-1/images/poster?tag=poster-tag");
    expect(imageUrl(item, "fanart")).toBe("/api/v1/items/item-1/images/fanart?tag=fanart-tag");
  });

  it("appends the display width after the tag query", () => {
    expect(imageUrl(item, "poster", 360)).toBe(
      "/api/v1/items/item-1/images/poster?tag=poster-tag&w=360",
    );
    expect(imageUrl(item, "fanart", 1280)).toBe(
      "/api/v1/items/item-1/images/fanart?tag=fanart-tag&w=1280",
    );
  });

  it("uses a different item id for poster fallbacks while keeping the width", () => {
    const episode = {
      ...item,
      id: "episode-1",
      posterItemId: "series-1",
      imageTags: { poster: "series-poster" },
    } as MediaItem;
    expect(imageUrl(episode, "poster", 720)).toBe(
      "/api/v1/items/series-1/images/poster?tag=series-poster&w=720",
    );
  });

  it("appends widths to still image URLs and handles both query shapes", () => {
    expect(withImageWidth("/api/v1/items/a/images/fanart?tag=t", 720)).toBe(
      "/api/v1/items/a/images/fanart?tag=t&w=720",
    );
    expect(withImageWidth("/api/v1/items/a/images/fanart", 720)).toBe(
      "/api/v1/items/a/images/fanart?w=720",
    );
    expect(withImageWidth("/api/v1/items/a/images/fanart?tag=t")).toBe(
      "/api/v1/items/a/images/fanart?tag=t",
    );
  });

  it("only returns a width while the effective capability is enabled", () => {
    expect(imageVariantWidth(true, 360)).toBe(360);
    expect(imageVariantWidth(false, 360)).toBeUndefined();
  });
});
