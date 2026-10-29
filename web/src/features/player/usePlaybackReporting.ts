import { useCallback, useEffect, useRef } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { PlaybackEventState } from "../../lib/api/types";
import { TICKS_PER_SECOND } from "./player-page-helpers";

export const PROGRESS_REPORT_INTERVAL_MS = 10_000;
export const PLAYBACK_HEARTBEAT_INTERVAL_MS = 60_000;

type VideoRef = { current: HTMLVideoElement | null };
type SessionIdRef = { current: string | null };

export type PlaybackReporting = {
  reportPlayback: (
    state: PlaybackEventState,
    force?: boolean,
    keepalive?: boolean,
    videoOverride?: HTMLVideoElement | null,
    sessionIdOverride?: string | null,
  ) => Promise<unknown> | undefined;
  stopActiveSession: (sessionId: string | null, keepalive?: boolean) => Promise<void>;
  markStarted: () => void;
  resetReportingClock: () => void;
  resetSequence: () => void;
};

type PlaybackReportingOptions = {
  sessionIdRef: SessionIdRef;
  videoRef: VideoRef;
  lastVideoRef: VideoRef;
  /** Current session id; only used to drive the 60s heartbeat. */
  sessionId: string | null;
};

/**
 * Owns the web playback telemetry surface: the throttled progress reporter, the
 * session stop request, the 60s heartbeat, and the pagehide/unmount flush.
 *
 * Sequencing and throttling semantics are load-bearing (the server rejects stale
 * sequences), so this hook keeps the exact same rules as the original inline
 * implementation in PlayerPage.
 */
export function usePlaybackReporting({
  sessionIdRef,
  videoRef,
  lastVideoRef,
  sessionId,
}: PlaybackReportingOptions): PlaybackReporting {
  const queryClient = useQueryClient();
  const lastProgressReportRef = useRef(0);
  const hasStartedRef = useRef(false);
  const playbackSequenceRef = useRef(0);

  const reportPlayback = useCallback(
    (
      state: PlaybackEventState,
      force = false,
      keepalive = false,
      videoOverride?: HTMLVideoElement | null,
      sessionIdOverride?: string | null,
    ) => {
      const video = videoOverride ?? videoRef.current;
      if (!video || (state === "STOPPED" && !hasStartedRef.current)) return undefined;
      const now = Date.now();
      if (!force && now - lastProgressReportRef.current < PROGRESS_REPORT_INTERVAL_MS) return undefined;
      const positionTicks = Math.max(
        0,
        Math.round(
          (Number.isFinite(video.currentTime) ? video.currentTime : 0) * TICKS_PER_SECOND,
        ),
      );
      const durationTicks =
        Number.isFinite(video.duration) && video.duration >= 0
          ? Math.round(video.duration * TICKS_PER_SECOND)
          : null;
      lastProgressReportRef.current = now;
      const sessionId = sessionIdOverride ?? sessionIdRef.current;
      if (!sessionId) return undefined;
      const sequence = ++playbackSequenceRef.current;
      const request = api.webPlaybackEvent(
        sessionId,
        {
          eventId: `web-${sessionId}-${sequence}-${now}`,
          sequence,
          state,
          positionTicks,
          durationTicks,
        },
        keepalive,
      );
      if (state === "STOPPED") {
        return request
          .then(() => {
            queryClient.removeQueries({
              predicate: (query) => query.queryKey[0] === "web-playback-session"
                && (query.state.data as { sessionId?: string } | undefined)?.sessionId === sessionId,
            });
            void queryClient.invalidateQueries({ queryKey: queryKeys.home });
          })
          .catch(() => undefined);
      } else {
        void request.catch(() => undefined);
        return request;
      }
    },
    [queryClient, sessionIdRef, videoRef],
  );

  const stopActiveSession = useCallback((sessionId: string | null, keepalive = false) => {
    if (!sessionId) return Promise.resolve();
    return api.stopWebPlaybackSession(sessionId, keepalive)
      .then(() => {
        queryClient.removeQueries({
          predicate: (query) => query.queryKey[0] === "web-playback-session"
            && (query.state.data as { sessionId?: string } | undefined)?.sessionId === sessionId,
        });
      })
      .catch(() => undefined);
  }, [queryClient]);

  const markStarted = useCallback(() => {
    hasStartedRef.current = true;
  }, []);

  const resetReportingClock = useCallback(() => {
    lastProgressReportRef.current = 0;
    hasStartedRef.current = false;
  }, []);

  const resetSequence = useCallback(() => {
    playbackSequenceRef.current = 0;
  }, []);

  useEffect(() => {
    if (!sessionId) return undefined;
    const heartbeat = window.setInterval(() => {
      void api.webPlaybackHeartbeat(sessionId).catch(() => undefined);
    }, PLAYBACK_HEARTBEAT_INTERVAL_MS);
    return () => window.clearInterval(heartbeat);
  }, [sessionId]);

  useEffect(() => {
    const handlePageHide = () => {
      const currentSessionId = sessionIdRef.current;
      void reportPlayback("STOPPED", true, true, undefined, currentSessionId);
      void stopActiveSession(currentSessionId, true);
    };
    window.addEventListener("pagehide", handlePageHide);
    return () => {
      window.removeEventListener("pagehide", handlePageHide);
      const currentSessionId = sessionIdRef.current;
      void reportPlayback("STOPPED", true, false, lastVideoRef.current, currentSessionId);
      void stopActiveSession(currentSessionId);
    };
  }, [lastVideoRef, reportPlayback, sessionIdRef, stopActiveSession]);

  return {
    reportPlayback,
    stopActiveSession,
    markStarted,
    resetReportingClock,
    resetSequence,
  };
}
