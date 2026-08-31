import { vi } from "vitest";
import { api } from "../src/lib/api/client";

export function mockPlaybackBootstrap() {
  return vi.spyOn(api, "createWebPlaybackBootstrap").mockImplementation(async (
    itemId,
    requestedSourceId,
    capabilities,
  ) => {
    const [item, playback] = await Promise.all([
      api.item(itemId),
      api.playback(itemId).catch(() => ({})),
    ]);
    const source = requestedSourceId
      ? item.mediaSources?.find((entry) => entry.id === requestedSourceId)
      : item.mediaSources?.find((entry) => entry.isDefault) ?? item.mediaSources?.[0];
    if (!source) throw new Error("test item has no playback source");
    const session = await api.createWebPlaybackSession(itemId, source.id, capabilities);
    return { item, playback, session };
  });
}
