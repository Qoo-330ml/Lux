export type SetupStatus = {
  initialized: boolean;
};

export type LuxUser = {
  id: string;
  usernameNormalized: string;
  displayName?: string | null;
  canManageServer?: boolean;
  canRemoteAccess?: boolean;
  canDownload?: boolean;
};

export type Library = {
  id: string;
  name: string;
  kind: "MOVIE" | "SERIES" | "MIXED" | string;
  itemCount?: number;
};

export type ImageTags = Partial<Record<"poster" | "fanart" | "backdrop", string>>;

export type UserData = {
  isPlayed?: boolean;
  isFavorite?: boolean;
  playbackPositionTicks?: number;
};

export type MediaSource = {
  id: string;
  qualityLabel?: string | null;
  editionName?: string | null;
  container?: string | null;
  durationTicks?: number | null;
  isDefault?: boolean;
};

export type MediaItem = {
  id: string;
  title?: string | null;
  name?: string | null;
  overview?: string | null;
  itemType?: "MOVIE" | "SERIES" | "SEASON" | "EPISODE" | "BOX_SET" | string;
  productionYear?: number | null;
  runtimeTicks?: number | null;
  imageTags?: ImageTags;
  userData?: UserData;
  mediaSources?: MediaSource[];
  parentId?: string | null;
  indexNumber?: number | null;
  parentIndexNumber?: number | null;
};

export type HomeResponse = {
  libraries?: Library[];
  continueWatching?: MediaItem[];
  recentlyAdded?: MediaItem[];
};

export type PageResponse<T> = {
  items?: T[];
  page?: number;
  pageSize?: number;
  total?: number;
};

export type PlaybackState = {
  isFavorite?: boolean;
  isPlayed?: boolean;
  positionTicks?: number;
  durationTicks?: number;
};

export type AdminRoot = {
  id: string;
  libraryId: string;
  canonicalPath: string;
  displayPath: string;
  isAvailable: boolean;
  isWritable: boolean;
  lastCheckedAt?: string | null;
  unavailableSince?: string | null;
  scanCursor?: string | null;
};

export type AdminLibrary = Library & {
  isEnabled: boolean;
  realtimeWatchEnabled: boolean;
  incrementalSchedule?: string | null;
  reconciliationSchedule?: string | null;
  metadataSchedule?: string | null;
  scanConcurrency?: number;
  probeConcurrency?: number;
  lastScanAt?: string | null;
  roots: AdminRoot[];
};

export type AdminUser = LuxUser & {
  isDisabled: boolean;
  isAdmin: boolean;
};

export type AdminHealth = {
  status: "ok" | "degraded" | string;
  schemaVersion: number;
  database: { status: string; journalMode: string; writable: boolean };
  config: { available: boolean; writable: boolean };
  ffprobe: { available: boolean };
  tmdb: { configured: boolean };
  jobs: {
    scanRunning: number;
    scanFailed: number;
    metadataReidentifyRunning: number;
  };
  libraries: Array<{
    id: string;
    name: string;
    isEnabled: boolean;
    rootCount: number;
    availableRootCount: number;
    writableRootCount: number;
  }>;
};

export type AdminJob = {
  id: string;
  libraryId: string;
  jobType: string;
  status: "PENDING" | "RUNNING" | "COMPLETED" | "CANCELLED" | "FAILED" | string;
  generation?: number;
  cursor?: string | null;
  processedCount?: number;
  totalCount?: number | null;
  cancelRequested?: boolean;
  error?: string | null;
};

export type AdminAuditEvent = {
  id: string;
  actorUserId?: string | null;
  actorUsername?: string | null;
  eventType: string;
  targetType?: string | null;
  targetId?: string | null;
  metadata?: Record<string, unknown>;
  createdAt: string;
};

export type AdminSettings = {
  resumePlayedPercent: number;
  resumeMinTicks: number;
};

export type ApiErrorBody = {
  error?: {
    code?: string;
    message?: string;
    requestId?: string;
  };
};
