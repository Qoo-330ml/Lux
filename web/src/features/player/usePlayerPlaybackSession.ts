import { useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type {
  MediaItem,
  MediaSource,
  PlaybackState,
  WebPlaybackSession,
  WebPlaybackTrackSelection,
} from "../../lib/api/types";
import { webPlaybackCapabilities } from "./playback-capabilities";
import { episodeNavigationFor } from "./player-page-helpers";
import type { PlayerEpisodeNavigation } from "./components/player-controls";

type PlayerPlaybackSessionOptions = {
  itemId: string;
  requestedSourceId: string | null;
  requestedAudioStreamIndex: string | null;
  requestedSubtitleStreamIndex: string | null;
  playbackAttempt: number;
  trackSelection: WebPlaybackTrackSelection | undefined;
};

export type PlayerPlaybackSession = {
  playbackBootstrap: ReturnType<typeof usePlaybackBootstrapQuery>;
  item: ReturnType<typeof useItemQuery>;
  playback: ReturnType<typeof usePlaybackQuery>;
  webPlaybackSession: ReturnType<typeof useWebPlaybackSessionQuery>;
  playbackData: PlaybackState | undefined;
  playbackDataRef: { current: PlaybackState | undefined };
  media: MediaItem | undefined;
  source: MediaSource | undefined;
  episodeNavigation: PlayerEpisodeNavigation | null;
  playbackSession: WebPlaybackSession | undefined;
  playbackPlan: WebPlaybackSession["plan"] | undefined;
  playbackSessionId: string | null;
  playbackKey: string;
  sessionGateKey: string;
  setSessionGateKey: (key: string) => void;
  sessionStartedRef: { current: boolean };
  playbackSessionIdRef: { current: string | null };
  useLegacyPlaybackQueries: boolean;
};

/**
 * Bootstrap/session/item queries for the player route, plus the derived playback
 * key and the session-gate key that serializes session replacement. The gate
 * effects stay in PlayerPage because they must run in page order relative to the
 * stop/heartbeat effects.
 */
export function usePlayerPlaybackSession({
  itemId,
  requestedSourceId,
  requestedAudioStreamIndex,
  requestedSubtitleStreamIndex,
  playbackAttempt,
  trackSelection,
}: PlayerPlaybackSessionOptions): PlayerPlaybackSession {
  const bootstrapKey = `${itemId}:${requestedSourceId ?? ""}:${requestedAudioStreamIndex ?? ""}:${playbackAttempt}`;
  const [sessionGateKey, setSessionGateKey] = useState(bootstrapKey);

  const playbackBootstrap = usePlaybackBootstrapQuery({
    itemId,
    requestedSourceId,
    requestedAudioStreamIndex,
    requestedSubtitleStreamIndex,
    playbackAttempt,
    trackSelection,
    bootstrapKey,
    sessionGateKey,
  });
  const useLegacyPlaybackQueries = playbackAttempt !== 0 || playbackBootstrap.isError;
  const item = useItemQuery(itemId, useLegacyPlaybackQueries);
  const playback = usePlaybackQuery(itemId, useLegacyPlaybackQueries);
  const playbackData = playbackBootstrap.isPlaceholderData
    ? playback.data
    : playbackBootstrap.data?.playback ?? playback.data;
  const playbackDataRef = useRef(playbackData);
  playbackDataRef.current = playbackData;

  const media = playbackBootstrap.data?.item ?? item.data;
  const episodeSiblings = useQuery({
    queryKey: queryKeys.children(media?.seriesId ?? "", "EPISODE", media?.parentId ?? undefined),
    queryFn: () => api.children(media?.seriesId ?? "", {
      itemType: "EPISODE",
      seasonId: media?.parentId ?? undefined,
    }),
    enabled: media?.itemType === "EPISODE" && Boolean(media.seriesId) && Boolean(media.parentId),
    staleTime: 60_000,
  });
  const episodeNavigation = media?.itemType === "EPISODE"
    ? episodeNavigationFor(episodeSiblings.data?.items ?? [], media.id)
    : null;
  const source =
    media?.mediaSources?.find((entry) => entry.id === requestedSourceId) ??
    media?.mediaSources?.find((entry) => entry.isDefault) ??
    media?.mediaSources?.[0];

  // For the first bootstrap request, keep the key tied to the URL selection
  // (or the default slot) rather than changing it when the response resolves
  // its default source. This prevents the newly-created bootstrap session from
  // being mistaken for an old session during the initial render.
  const playbackKeySourceId = playbackAttempt === 0 && !requestedSourceId
    ? ""
    : source?.id ?? requestedSourceId ?? "";
  const playbackKey = `${itemId}:${playbackKeySourceId}:${requestedAudioStreamIndex ?? ""}:${playbackAttempt}`;
  const sessionStartedRef = useRef(false);
  const playbackSessionIdRef = useRef<string | null>(null);
  const webPlaybackSession = useWebPlaybackSessionQuery({
    itemId,
    source,
    playbackAttempt,
    requestedAudioStreamIndex,
    requestedSubtitleStreamIndex,
    trackSelection,
    useLegacyPlaybackQueries,
    playbackKey,
    sessionGateKey,
    sessionStartedRef,
    playbackSessionIdRef,
  });
  const playbackSession = playbackBootstrap.isPlaceholderData
    ? webPlaybackSession.data
    : playbackBootstrap.data?.session ?? webPlaybackSession.data;
  const playbackPlan = playbackSession?.plan;
  const playbackSessionId = playbackSession?.sessionId ?? null;

  return {
    playbackBootstrap,
    item,
    playback,
    webPlaybackSession,
    playbackData,
    playbackDataRef,
    media,
    source,
    episodeNavigation,
    playbackSession,
    playbackPlan,
    playbackSessionId,
    playbackKey,
    sessionGateKey,
    setSessionGateKey,
    sessionStartedRef,
    playbackSessionIdRef,
    useLegacyPlaybackQueries,
  };
}

type BootstrapQueryOptions = PlayerPlaybackSessionOptions & {
  bootstrapKey: string;
  sessionGateKey: string;
};

function usePlaybackBootstrapQuery({
  itemId,
  requestedSourceId,
  requestedAudioStreamIndex,
  requestedSubtitleStreamIndex,
  playbackAttempt,
  trackSelection,
  bootstrapKey,
  sessionGateKey,
}: BootstrapQueryOptions) {
  const bootstrapCapabilities = webPlaybackCapabilities(undefined, playbackAttempt);
  return useQuery({
    queryKey: queryKeys.playbackBootstrap(itemId, requestedSourceId, playbackAttempt, requestedAudioStreamIndex, requestedSubtitleStreamIndex),
    queryFn: ({ signal }) => api.createWebPlaybackBootstrap(
      itemId,
      requestedSourceId ?? undefined,
      bootstrapCapabilities,
      signal,
      trackSelection,
    ),
    enabled: Boolean(itemId) && playbackAttempt === 0 && sessionGateKey === bootstrapKey,
    retry: false,
    staleTime: Number.POSITIVE_INFINITY,
    placeholderData: (previous) => previous,
  });
}

function useItemQuery(itemId: string, enabled: boolean) {
  return useQuery({
    queryKey: queryKeys.item(itemId),
    queryFn: () => api.item(itemId),
    enabled: Boolean(itemId) && enabled,
  });
}

function usePlaybackQuery(itemId: string, enabled: boolean) {
  return useQuery({
    queryKey: queryKeys.playback(itemId),
    queryFn: () => api.playback(itemId),
    enabled: Boolean(itemId) && enabled,
  });
}

type WebPlaybackSessionQueryOptions = {
  itemId: string;
  source: MediaSource | undefined;
  playbackAttempt: number;
  requestedAudioStreamIndex: string | null;
  requestedSubtitleStreamIndex: string | null;
  trackSelection: WebPlaybackTrackSelection | undefined;
  useLegacyPlaybackQueries: boolean;
  playbackKey: string;
  sessionGateKey: string;
  sessionStartedRef: { current: boolean };
  playbackSessionIdRef: { current: string | null };
};

function useWebPlaybackSessionQuery({
  itemId,
  source,
  playbackAttempt,
  requestedAudioStreamIndex,
  requestedSubtitleStreamIndex,
  trackSelection,
  useLegacyPlaybackQueries,
  playbackKey,
  sessionGateKey,
  sessionStartedRef,
  playbackSessionIdRef,
}: WebPlaybackSessionQueryOptions) {
  const capabilities = webPlaybackCapabilities(source, playbackAttempt);
  return useQuery({
    queryKey: queryKeys.webPlaybackSession(itemId, source?.id ?? "", playbackAttempt, requestedAudioStreamIndex, requestedSubtitleStreamIndex),
    queryFn: ({ signal }) => api.createWebPlaybackSession(
      itemId,
      source?.id ?? "",
      capabilities,
      signal,
      trackSelection,
    ),
    enabled: Boolean(itemId && source?.id) && useLegacyPlaybackQueries
      && (sessionGateKey === playbackKey
        || (!sessionStartedRef.current && playbackSessionIdRef.current === null)),
    retry: false,
    staleTime: Number.POSITIVE_INFINITY,
  });
}
