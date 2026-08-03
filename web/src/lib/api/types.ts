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

export type ApiErrorBody = {
  error?: {
    code?: string;
    message?: string;
    requestId?: string;
  };
};
