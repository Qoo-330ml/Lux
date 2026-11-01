import { useMemo, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type {
  MediaItem,
  MediaSource,
  PlaybackState,
  WebPlaybackRequestSelection,
  WebPlaybackSession,
} from "../../lib/api/types";
import { parseMaxBitrateParam } from "./playback-quality";
import { webPlaybackCapabilities } from "./playback-capabilities";
import { episodeNavigationFor } from "./player-page-helpers";
import type { PlayerEpisodeNavigation } from "./components/player-controls";

type PlayerPlaybackSessionOptions = {
  itemId: string;
  requestedSourceId: string | null;
  requestedAudioStreamIndex: string | null;
  requestedSubtitleStreamIndex: string | null;
  requestedMaxBitrate: string | null;
  playbackAttempt: number;
  /**
   * The previous native Direct Play attempt decoded no audio at all, so the fallback request must
   * not offer the audio track as a copy again.
   */
  forceAudioTranscode?: boolean;
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
  requestedMaxBitrate,
  playbackAttempt,
  forceAudioTranscode = false,
}: PlayerPlaybackSessionOptions): PlayerPlaybackSession {
  // Out-of-contract URL values are dropped instead of being sent: the API
  // rejects them with 400 and the player would have no recoverable UI state.
  const maxBitrate = parseMaxBitrateParam(requestedMaxBitrate);
  const requestSelection = useMemo<WebPlaybackRequestSelection | undefined>(() => {
    const audioStreamIndex = requestedAudioStreamIndex?.trim() ? Number(requestedAudioStreamIndex) : undefined;
    const subtitleStreamIndex = requestedSubtitleStreamIndex?.trim() ? Number(requestedSubtitleStreamIndex) : undefined;
    return Number.isInteger(audioStreamIndex) || Number.isInteger(subtitleStreamIndex) || maxBitrate !== null
      ? {
        ...(Number.isInteger(audioStreamIndex) ? { audioStreamIndex } : {}),
        ...(Number.isInteger(subtitleStreamIndex) ? { subtitleStreamIndex } : {}),
        ...(maxBitrate !== null ? { maxBitrate } : {}),
      }
      : undefined;
  }, [maxBitrate, requestedAudioStreamIndex, requestedSubtitleStreamIndex]);
  const bootstrapKey = `${itemId}:${requestedSourceId ?? ""}:${requestedAudioStreamIndex ?? ""}:${maxBitrate ?? ""}:${playbackAttempt}`;
  const [sessionGateKey, setSessionGateKey] = useState(bootstrapKey);

  const playbackBootstrap = usePlaybackBootstrapQuery({
    itemId,
    requestedSourceId,
    requestedAudioStreamIndex,
    requestedSubtitleStreamIndex,
    maxBitrate,
    playbackAttempt,
    requestSelection,
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
  const playbackKey = `${itemId}:${playbackKeySourceId}:${requestedAudioStreamIndex ?? ""}:${maxBitrate ?? ""}:${playbackAttempt}`;
  const sessionStartedRef = useRef(false);
  const playbackSessionIdRef = useRef<string | null>(null);
  const webPlaybackSession = useWebPlaybackSessionQuery({
    itemId,
    source,
    playbackAttempt,
    requestedAudioStreamIndex,
    requestedSubtitleStreamIndex,
    maxBitrate,
    requestSelection,
    useLegacyPlaybackQueries,
    playbackKey,
    sessionGateKey,
    sessionStartedRef,
    playbackSessionIdRef,
    forceAudioTranscode,
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

type BootstrapQueryOptions = Omit<PlayerPlaybackSessionOptions, "requestedMaxBitrate"> & {
  maxBitrate: number | null;
  requestSelection: WebPlaybackRequestSelection | undefined;
  bootstrapKey: string;
  sessionGateKey: string;
};

function usePlaybackBootstrapQuery({
  itemId,
  requestedSourceId,
  requestedAudioStreamIndex,
  requestedSubtitleStreamIndex,
  maxBitrate,
  playbackAttempt,
  requestSelection,
  bootstrapKey,
  sessionGateKey,
}: BootstrapQueryOptions) {
  const bootstrapCapabilities = webPlaybackCapabilities(undefined, playbackAttempt);
  return useQuery({
    queryKey: queryKeys.playbackBootstrap(
      itemId,
      requestedSourceId,
      playbackAttempt,
      requestedAudioStreamIndex,
      requestedSubtitleStreamIndex,
      maxBitrate,
    ),
    queryFn: ({ signal }) => api.createWebPlaybackBootstrap(
      itemId,
      requestedSourceId ?? undefined,
      bootstrapCapabilities,
      signal,
      requestSelection,
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
  maxBitrate: number | null;
  requestSelection: WebPlaybackRequestSelection | undefined;
  useLegacyPlaybackQueries: boolean;
  playbackKey: string;
  sessionGateKey: string;
  sessionStartedRef: { current: boolean };
  playbackSessionIdRef: { current: string | null };
  forceAudioTranscode: boolean;
};

function useWebPlaybackSessionQuery({
  itemId,
  source,
  playbackAttempt,
  requestedAudioStreamIndex,
  requestedSubtitleStreamIndex,
  maxBitrate,
  requestSelection,
  useLegacyPlaybackQueries,
  playbackKey,
  sessionGateKey,
  sessionStartedRef,
  playbackSessionIdRef,
  forceAudioTranscode,
}: WebPlaybackSessionQueryOptions) {
  // A retry that follows a silent native track declares the audio track non-copyable, so the
  // server's existing capabilities decision picks the audio-transcode tier.
  const capabilities = webPlaybackCapabilities(source, playbackAttempt, undefined, { forceAudioTranscode });
  return useQuery({
    queryKey: queryKeys.webPlaybackSession(
      itemId,
      source?.id ?? "",
      playbackAttempt,
      requestedAudioStreamIndex,
      requestedSubtitleStreamIndex,
      maxBitrate,
    ),
    queryFn: ({ signal }) => api.createWebPlaybackSession(
      itemId,
      source?.id ?? "",
      capabilities,
      signal,
      requestSelection,
    ),
    enabled: Boolean(itemId && source?.id) && useLegacyPlaybackQueries
      && (sessionGateKey === playbackKey
        || (!sessionStartedRef.current && playbackSessionIdRef.current === null)),
    retry: false,
    staleTime: Number.POSITIVE_INFINITY,
  });
}
