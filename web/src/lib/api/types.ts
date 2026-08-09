export type SetupStatus = {
  initialized: boolean;
};

export type SetupDatabaseBackend = "SQLITE" | "POSTGRESQL";

export type SetupDatabaseStatus = {
  configured: boolean;
  backend?: SetupDatabaseBackend | null;
  currentBackend: SetupDatabaseBackend;
  restartRequired: boolean;
};

export type DatabaseSetupInput =
  | { backend: "SQLITE" }
  | {
      backend: "POSTGRESQL";
      host: string;
      port: number;
      database: string;
      username: string;
      password: string;
      sslMode: "disable" | "prefer" | "require" | "verify-ca" | "verify-full";
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

export type ImageTags = Partial<Record<"poster" | "fanart" | "backdrop" | "thumb", string>>;

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
  premiereDate?: string | null;
  lastAirDate?: string | null;
  status?: string | null;
  originalLanguage?: string | null;
  productionYear?: number | null;
  rating?: number | null;
  ratingSource?: string | null;
  providerIds?: Record<string, string> | null;
  seasonCount?: number | null;
  episodeCount?: number | null;
  runtimeTicks?: number | null;
  imageTags?: ImageTags;
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
  continueWatchingTotal?: number;
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
  state?: "PLAYING" | "PAUSED";
  isPaused?: boolean;
  lastEventAt?: number | null;
};

export type PlaybackEventState = "PLAYING" | "PAUSED" | "STOPPED";

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
  /** @deprecated Realtime incremental scans are event-driven and have no schedule. */
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
  configValues?: Record<string, unknown>;
  configSource: "BUILT_IN" | "CUSTOM" | "ENVIRONMENT" | "READ_ACCESS_TOKEN" | "NONE" | string;
};

export type AdminPluginConfigField = {
  key: string;
  label: string;
  type: "password" | "text" | "select" | "toggle" | "number" | string;
  required: boolean;
  sensitive: boolean;
  description?: string | null;
  multiple?: boolean;
  optionsSource?: string | null;
  defaultValue?: unknown;
  minimum?: number | null;
  maximum?: number | null;
  options?: Array<{ value: string; label: string }>;
};

export type AdminUser = LuxUser & {
  isDisabled: boolean;
  isAdmin: boolean;
};

export type AdminHealth = {
  status: "ok" | "degraded" | string;
  schemaVersion: number;
  runtime: { seconds: number };
  resources: {
    cpu: {
      available: boolean;
      source: string;
      usagePercent: number | null;
      limitCores: number | null;
    };
    memory: {
      available: boolean;
      source: string;
      usedBytes: number | null;
      limitBytes: number | null;
      usagePercent: number | null;
    };
    mediaStorage: {
      available: boolean;
      source: string;
      path: string;
      totalBytes: number | null;
      usedBytes: number | null;
      availableBytes: number | null;
      usagePercent: number | null;
    };
  };
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

export type AdminDashboard = {
  server: {
    name: string;
    version: string;
    commit: string;
    schemaVersion: number;
  };
  stats: {
    movieCount: number;
    seriesCount: number;
    userCount: number;
  };
  health: AdminHealth;
  nowPlaying: AdminPlaybackSession[];
  activity: AdminActivityEvent[];
};

export type AdminPlaybackSession = {
  id: string;
  userId: string;
  userName: string;
  itemId: string;
  title: string;
  originalTitle?: string | null;
  itemType: string;
  seriesId?: string | null;
  seriesTitle?: string | null;
  productionYear?: number | null;
  parentIndexNumber?: number | null;
  indexNumber?: number | null;
  posterAvailable: boolean;
  positionTicks: number;
  durationTicks?: number | null;
  state: "PLAYING" | "PAUSED" | string;
  isPaused: boolean;
  lastEventAt: number;
  client?: string | null;
  clientVersion?: string | null;
  deviceId: string;
  deviceName?: string | null;
  deviceType?: string | null;
  remoteIp?: string | null;
  remoteIpLocation?: AdminIpLocation | null;
  playSessionId: string;
  source?: AdminPlaybackSource | null;
};
export type AdminIpLocation = {
  location?: string | null;
  district?: string | null;
  street?: string | null;
  isp?: string | null;
};

export type AdminPlaybackSource = {
  id: string;
  qualityLabel?: string | null;
  editionName?: string | null;
  container?: string | null;
  bitrate?: number | null;
  durationTicks?: number | null;
  video?: { codec?: string | null; title?: string | null; details?: Record<string, unknown> } | null;
  audio?: { codec?: string | null; language?: string | null; title?: string | null } | null;
};

export type AdminActivityEvent = {
  id: string;
  userId?: string | null;
  userName?: string | null;
  eventType: "AUTH_LOGIN" | "PLAYBACK_STARTED" | "PLAYBACK_PAUSED" | "PLAYBACK_STOPPED" | string;
  targetType?: string | null;
  targetId?: string | null;
  targetTitle?: string | null;
  metadata?: Record<string, unknown>;
  createdAt: number;
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
  createdAt?: string | number;
};

export type AdminScheduledTask = {
  id?: string;
  ownerType: "GLOBAL" | "LIBRARY" | string;
  ownerId: string;
  ownerName?: string | null;
  taskType: string;
  name?: string | null;
  description?: string | null;
  sourceType?: "SYSTEM" | "PLUGIN" | string;
  pluginId?: string | null;
  schedule?: string | null;
  isEnabled: boolean;
  resourceLimit?: Record<string, unknown>;
  createdAt?: string | number;
  updatedAt?: string | number;
};

export type AdminScheduledTaskPage = {
  scheduledTasks?: AdminScheduledTask[];
  total?: number;
  page?: number;
  pageSize?: number;
};

export type AdminMetadataReidentifyJob = {
  id: string;
  status: "QUEUED" | "RUNNING" | "COMPLETED" | "CANCELLED" | "FAILED" | string;
  cancelRequested?: boolean;
  mode: "REIDENTIFY" | "FILL_MISSING" | "FULL_REFRESH" | string;
  processedCount: number;
  totalCount: number;
  error?: string | null;
  createdAt: string | number;
  updatedAt?: string | number;
  startedAt?: string | number | null;
  finishedAt?: string | number | null;
};

export type AdminMetadataReidentifyStart = {
  totalCount: number;
  mode?: MetadataRefreshMode;
  job: AdminMetadataReidentifyJob;
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
  createdAt: string | number;
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
  serverName?: string;
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
