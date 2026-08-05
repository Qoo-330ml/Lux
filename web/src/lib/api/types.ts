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
  coverImageUrl?: string | null;
  itemCount?: number;
  latest?: MediaItem[];
};

export type ImageTags = Partial<Record<"poster" | "fanart" | "backdrop", string>>;

export type UserData = {
  isPlayed?: boolean;
  isFavorite?: boolean;
  positionTicks?: number;
  /** @deprecated Older Web clients used this name; prefer positionTicks. */
  playbackPositionTicks?: number;
};

export type MediaStream = {
  index: number;
  type?: string | null;
  codec?: string | null;
  language?: string | null;
  title?: string | null;
  isExternal?: boolean;
  isDefault?: boolean;
  isForced?: boolean;
  details?: Record<string, unknown>;
};

export type MediaSource = {
  id: string;
  sourceKind?: string | null;
  qualityLabel?: string | null;
  editionName?: string | null;
  container?: string | null;
  size?: number | null;
  bitrate?: number | null;
  durationTicks?: number | null;
  externalUrl?: string | null;
  probeStatus?: string | null;
  isDefault?: boolean;
  streams?: MediaStream[];
};

export type MediaActor = {
  id: string;
  name: string;
  character?: string | null;
  imageUrl?: string | null;
};

export type MediaItem = {
  id: string;
  title?: string | null;
  name?: string | null;
  originalTitle?: string | null;
  overview?: string | null;
  itemType?: "MOVIE" | "SERIES" | "SEASON" | "EPISODE" | "BOX_SET" | string;
  productionYear?: number | null;
  premiereDate?: string | null;
  rating?: number | null;
  ratingSource?: string | null;
  runtimeTicks?: number | null;
  imageTags?: ImageTags;
  providerIds?: Record<string, string> | null;
  seasonCount?: number | null;
  episodeCount?: number | null;
  userData?: UserData;
  mediaSources?: MediaSource[];
  actors?: MediaActor[];
  parentId?: string | null;
  seriesId?: string | null;
  indexNumber?: number | null;
  parentIndexNumber?: number | null;
};

export type MetadataFieldName = "title" | "originalTitle" | "overview" | "productionYear";

export type ItemMetadata = {
  title: string;
  originalTitle?: string | null;
  overview?: string | null;
  productionYear?: number | null;
  lockedFields: MetadataFieldName[];
};

export type ItemImage = {
  id: string;
  itemId: string;
  imageType: string;
  imageIndex: number;
  fileSize?: number | null;
  contentTag?: string | null;
  source?: string | null;
  language?: string | null;
  url: string;
};

export type ImageSearchResult = {
  id: string;
  imageType: string;
  imageIndex: number;
  language?: string | null;
  width?: number | null;
  height?: number | null;
  source: string;
  url: string;
};

export type HomeResponse = {
  libraries?: Library[];
  recommended?: MediaItem[];
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
  scraperId?: string | null;
  isEnabled: boolean;
  realtimeWatchEnabled: boolean;
  incrementalSchedule?: string | null;
  reconciliationSchedule?: string | null;
  metadataSchedule?: string | null;
  mediaStrategy?: MediaStrategySettings | null;
  scanConcurrency?: number;
  probeConcurrency?: number;
  lastScanAt?: string | null;
  roots: AdminRoot[];
};

export type AdminPlugin = {
  id: string;
  name: string;
  description: string;
  category: string;
  version?: string | null;
  runtime?: string | null;
  capabilities?: string[];
  status?: string;
  running?: boolean;
  lastError?: string | null;
  installed: boolean;
  enabled: boolean;
  configured: boolean;
  available: boolean;
  unavailableReason?: string | null;
  configurable: boolean;
  configFields: AdminPluginConfigField[];
  configSource: "BUILT_IN" | "CUSTOM" | "ENVIRONMENT" | "READ_ACCESS_TOKEN" | "NONE" | string;
};

export type AdminPluginConfigField = {
  key: string;
  label: string;
  type: "password" | "text" | string;
  required: boolean;
  sensitive: boolean;
  description: string;
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

export type AdminMetadataReidentifyBatch = {
  totalCount: number;
  jobCount: number;
  mode?: MetadataRefreshMode;
  jobs: Array<Pick<AdminJob, "id" | "status" | "totalCount">>;
};

export type MetadataRefreshMode = "FILL_MISSING" | "FULL_REFRESH";

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

export type AdminMetadataCandidate = {
  id: string;
  itemId: string;
  itemTitle: string;
  provider: string;
  providerId: string;
  candidate: Record<string, unknown>;
  score: number;
  status: string;
  expiresAt?: string | null;
  fieldDiffs: Array<{ field: string; current?: unknown; candidate?: unknown; provenance?: unknown }>;
};

export type AdminImage = {
  id: string;
  itemId: string;
  imageType: string;
  imageIndex: number;
  fileSize?: number | null;
  contentTag?: string | null;
  source?: string | null;
};

export type AdminSettings = {
  resumePlayedPercent: number;
  resumeMinTicks: number;
  mediaStrategy: MediaStrategySettings;
  networkProxy?: AdminNetworkProxySettings;
};

export type AdminNetworkProxySettings = {
  configured: boolean;
  url: string | null;
  hasCredentials: boolean;
  source: "settings" | "environment" | "none" | string;
  restartRequired: boolean;
};

export type NetworkProxyDiagnostics = {
  proxySource: "settings" | "environment" | "none" | "input" | string;
  probes: NetworkProxyProbe[];
  egressIp: string | null;
  egressCountry: string | null;
};

export type NetworkProxyProbe = {
  id: string;
  label: string;
  latencyMs: number | null;
  status: number | null;
  reachable: boolean;
  error: string | null;
};

export type AdminSettingsPatch = Partial<AdminSettings> & {
  networkProxyUrl?: string | null;
};

export type MediaStrategySettings = {
  metadataLanguage: string;
  imageLanguage: string;
  region: string;
  scraperId?: string | null;
  metadataRefreshMode?: MetadataRefreshMode;
  applyScope: "NEW_CONTENT" | "SELECTED_CONTENT" | "ALL_CONTENT" | string;
  images: MediaImageStrategySettings;
  subtitles: MediaSubtitleStrategySettings;
};

export type MediaImageStrategySettings = {
  poster: boolean;
  artwork: boolean;
  banner: boolean;
  logo: boolean;
  thumbnail: boolean;
  disc: boolean;
  wallpaper: boolean;
  maxBackdropCount: number;
  minDownloadWidth: number;
};

export type MediaSubtitleStrategySettings = {
  autoDownload: boolean;
  languages: string[];
  forcedOnly: boolean;
  hearingImpaired: boolean;
};

export type ApiErrorBody = {
  error?: {
    code?: string;
    message?: string;
    requestId?: string;
  };
};
