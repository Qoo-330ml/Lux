export const queryKeys = {
  setup: ["setup"] as const,
  me: ["me"] as const,
  home: ["home"] as const,
  libraries: ["libraries"] as const,
  library: (libraryId: string, page: number) =>
    ["library", libraryId, page] as const,
  item: (itemId: string) => ["item", itemId] as const,
  playback: (itemId: string) => ["playback", itemId] as const,
};
