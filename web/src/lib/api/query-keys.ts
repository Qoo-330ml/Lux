export const queryKeys = {
  setup: ["setup"] as const,
  me: ["me"] as const,
  home: ["home"] as const,
  libraries: ["libraries"] as const,
  library: (libraryId: string, page: number) =>
    ["library", libraryId, page] as const,
  item: (itemId: string) => ["item", itemId] as const,
  playback: (itemId: string) => ["playback", itemId] as const,
  adminHealth: ["admin", "health"] as const,
  adminLibraries: ["admin", "libraries"] as const,
  adminUsers: ["admin", "users"] as const,
  adminJobs: (status?: string) => ["admin", "jobs", status ?? "all"] as const,
  adminLogs: ["admin", "logs"] as const,
  adminSettings: ["admin", "settings"] as const,
};
