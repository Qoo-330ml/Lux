import { describe, expect, it } from "vitest";
import { detailBackdropUrl } from "../src/features/home/media-display";
import type { MediaItem } from "../src/lib/api/types";

const item = (id: string, itemType: string, imageTags: MediaItem["imageTags"] = {}, extra: Partial<MediaItem> = {}) =>
  ({ id, itemType, imageTags, ...extra }) as MediaItem;

describe("detailBackdropUrl", () => {
  const series = item("series-1", "SERIES", { fanart: "sf", poster: "sp" });

  it("uses the movie's own fanart, then another landscape image, then the poster", () => {
    expect(detailBackdropUrl(item("m", "MOVIE", { fanart: "a", thumb: "b", poster: "c" }))).toBe("/api/v1/items/m/images/fanart?tag=a");
    expect(detailBackdropUrl(item("m", "MOVIE", { thumb: "b", poster: "c" }))).toBe("/api/v1/items/m/images/thumb?tag=b");
    expect(detailBackdropUrl(item("m", "MOVIE", { poster: "c" }))).toBe("/api/v1/items/m/images/poster?tag=c");
    expect(detailBackdropUrl(item("m", "MOVIE"))).toBeUndefined();
  });

  it("uses the series artwork for series, seasons and episodes", () => {
    const season = item("s", "SEASON", { fanart: "own", poster: "own-p" });
    const episode = item("e", "EPISODE", { fanart: "own", thumb: "still" });
    expect(detailBackdropUrl(series, series)).toBe("/api/v1/items/series-1/images/fanart?tag=sf");
    expect(detailBackdropUrl(season, series)).toBe("/api/v1/items/series-1/images/fanart?tag=sf");
    expect(detailBackdropUrl(episode, series)).toBe("/api/v1/items/series-1/images/fanart?tag=sf");
  });

  it("prefers any landscape image over a poster across the series and the item", () => {
    const posterOnlySeries = item("series-1", "SERIES", { poster: "sp" });
    const episode = item("e", "EPISODE", { thumb: "still", poster: "ep" });
    expect(detailBackdropUrl(episode, posterOnlySeries)).toBe("/api/v1/items/e/images/thumb?tag=still");
    expect(detailBackdropUrl(item("e", "EPISODE", { poster: "ep" }), posterOnlySeries)).toBe("/api/v1/items/series-1/images/poster?tag=sp");
  });

  it("waits for the series instead of flashing the episode still", () => {
    const episode = item("e", "EPISODE", { thumb: "still" }, { seriesId: "series-1" });
    expect(detailBackdropUrl(episode, undefined)).toBeUndefined();
    expect(detailBackdropUrl(item("e2", "EPISODE", { thumb: "still" }), undefined)).toBe("/api/v1/items/e2/images/thumb?tag=still");
  });
});
