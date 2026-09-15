import { describe, expect, it } from "vitest";
import type {
  HomeResponse,
  MediaActor,
  MediaItem,
  PlaybackState,
} from "../src/lib/api/types";

describe("Lux API response types", () => {
  it("model fields emitted by the home, catalog, people, and playback APIs", () => {
    const home: HomeResponse = { recentlyAddedTotal: 4 };
    const userData = { playCount: 2 };
    const item: MediaItem = {
      id: "item-1",
      sortTitle: "example",
      userData,
    };
    const actor: MediaActor = {
      id: "person-1",
      name: "Example Person",
      dateCreated: 1_700_000_000,
    };
    const playback: PlaybackState = {
      itemId: item.id,
      playCount: userData.playCount,
    };

    expect({
      recentlyAddedTotal: home.recentlyAddedTotal,
      sortTitle: item.sortTitle,
      playCount: item.userData?.playCount,
      dateCreated: actor.dateCreated,
      playbackItemId: playback.itemId,
      playbackCount: playback.playCount,
    }).toEqual({
      recentlyAddedTotal: 4,
      sortTitle: "example",
      playCount: 2,
      dateCreated: 1_700_000_000,
      playbackItemId: "item-1",
      playbackCount: 2,
    });
  });
});
