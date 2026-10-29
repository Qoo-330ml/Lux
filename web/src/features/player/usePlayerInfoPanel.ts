import { useEffect, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { MediaSource, WebPlaybackPlan } from "../../lib/api/types";
import type { PlaybackEngine } from "./playback-engine";
import {
  buildPlayerInfoGroups,
  createClientStatsSampler,
  summarizeSource,
  type PlayerClientStats,
  type PlayerInfoGroup,
} from "./player-stats";

type Ref<T> = { current: T };

export type PlayerInfoPanel = {
  infoGroups: PlayerInfoGroup[];
};

type PlayerInfoPanelOptions = {
  playbackSessionId: string | null;
  showInfo: boolean;
  playbackKey: string;
  playbackPlan: WebPlaybackPlan | undefined;
  tier: number | null;
  source: MediaSource | undefined;
  engineRef: Ref<PlaybackEngine | null>;
  videoRef: Ref<HTMLVideoElement | null>;
  lastVideoRef: Ref<HTMLVideoElement | null>;
  currentTime: number;
  duration: number;
  playbackRate: number;
  volume: number;
  isMuted: boolean;
  playing: boolean;
};

/**
 * Diagnostics panel data. The server row is polled once per second and the client
 * sampler ticks on the same cadence, but both only run while the panel is open —
 * the information panel never touches media files and does nothing while closed.
 */
export function usePlayerInfoPanel({
  playbackSessionId,
  showInfo,
  playbackKey,
  playbackPlan,
  tier,
  source,
  engineRef,
  videoRef,
  lastVideoRef,
  currentTime,
  duration,
  playbackRate,
  volume,
  isMuted,
  playing,
}: PlayerInfoPanelOptions): PlayerInfoPanel {
  const [clientStats, setClientStats] = useState<PlayerClientStats | null>(null);
  const infoStats = useQuery({
    queryKey: queryKeys.webPlaybackStats(playbackSessionId ?? ""),
    queryFn: ({ signal }) => api.webPlaybackStats(playbackSessionId ?? "", signal),
    enabled: showInfo && Boolean(playbackSessionId),
    retry: false,
    refetchInterval: 1_000,
    staleTime: 0,
  });

  useEffect(() => {
    if (!showInfo) {
      setClientStats(null);
      return;
    }
    const sampler = createClientStatsSampler();
    const sample = () => {
      setClientStats(sampler({
        engine: engineRef.current,
        video: videoRef.current ?? lastVideoRef.current,
      }));
    };
    sample();
    const interval = window.setInterval(() => {
      // Pure display data: nothing can be seen while the tab is hidden, and the
      // next visible tick refreshes the panel within one second.
      if (typeof document !== "undefined" && document.hidden) return;
      sample();
    }, 1_000);
    return () => window.clearInterval(interval);
  }, [showInfo, playbackKey]);

  const infoGroups = showInfo
    ? buildPlayerInfoGroups({
      plan: playbackPlan?.type ?? null,
      tier,
      source: summarizeSource(source),
      server: infoStats.data ?? null,
      serverPending: infoStats.isPending,
      serverError: infoStats.error instanceof Error ? infoStats.error.message : null,
      client: clientStats,
      playback: {
        positionSeconds: currentTime,
        durationSeconds: duration,
        rate: playbackRate,
        volume,
        muted: isMuted,
        paused: !playing,
      },
    })
    : [];

  return { infoGroups };
}
