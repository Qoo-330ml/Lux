import { useCallback, useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import type { PlaybackReporting } from "./usePlaybackReporting";

const RESUME_TIMEOUT_MS = 15_000;
const PLAYBACK_QUERY_PREFIXES = new Set(["playback-bootstrap", "web-playback-session"]);

type PendingResume = {
  /** Identifies the switch so delayed callbacks from a stale attempt are ignored. */
  sequence: number;
  itemId: string;
  /** URL value that should appear once the new session has been requested. */
  targetParam: string | null;
  position: number;
  playbackRate: number;
  volume: number;
  muted: boolean;
  playing: boolean;
  /** URL value to restore when the new session cannot be created. */
  previousParam: string | null;
  /** Stream URL when the switch started; used to ignore the old media. */
  originStreamUrl: string;
  /** Set once the URL has switched to the target; later key changes cancel. */
  appliedPlaybackKey: string | null;
};

/** Copy shown to the user when a switch is abandoned; supplied per variant. */
export type PlaybackVariantSwitchMessages = {
  timeout: string;
  failure: string;
  autoplayBlocked: string;
};

export type PlaybackVariantSwitchOptions = {
  itemId: string;
  playbackKey: string;
  /** URL parameter selecting this variant, e.g. `audioStreamIndex` or `maxBitrate`. */
  param: string;
  /** Current URL value of `param` (raw, may be whitespace); `null` when absent. */
  requestedValue: string | null;
  /**
   * Reduces a raw URL value to the exact form the playback query keys store, so
   * cache predicates compare like with like. Variants stored verbatim after a
   * trim (stream indexes) keep the default; variants stored as a parsed number
   * (the bitrate ceiling) pass their own canonicalizer, otherwise a URL such as
   * `maxBitrate=1e7` would look like "another variant" and evict the session
   * currently on screen.
   */
  canonicalValue?: (value: string | null) => string | null;
  /** Reads the same variant from a playback query key, used for cache invalidation. */
  queryVariant: (key: readonly unknown[]) => string;
  searchParams: URLSearchParams;
  setSearchParams: (next: URLSearchParams) => void;
  streamUrl: string;
  playing: boolean;
  playbackRate: number;
  sessionFailed: boolean;
  videoRef: { current: HTMLVideoElement | null };
  reportPlayback: PlaybackReporting["reportPlayback"];
  resetControlsTimeout: () => void;
  messages: PlaybackVariantSwitchMessages;
};

export type PlaybackVariantSwitch = {
  switching: boolean;
  notice: string | null;
  /**
   * Rebuilds the playback session for the variant encoded in `nextParams`.
   * Callers validate the target first; this only runs the shared choreography:
   * persist the live position, rewrite the URL, restore position/speed/paused
   * state on the replacement media without unmounting `<video>`, and roll back
   * when the new session fails or times out.
   */
  beginSwitch: (nextParams: URLSearchParams) => void;
};

function normalizeParam(value: string | null | undefined): string | null {
  return value?.trim() || null;
}

function isPlaybackQueryFor(
  queryKey: readonly unknown[],
  itemId: string,
  variant: string,
  queryVariant: (key: readonly unknown[]) => string,
): boolean {
  const [prefix, queryItemId] = queryKey;
  return queryItemId === itemId
    && PLAYBACK_QUERY_PREFIXES.has(String(prefix))
    && queryVariant(queryKey) === variant;
}

/**
 * Shared "rebuild the session for another variant" machinery used by the audio
 * track and quality switches. Both variants are ordinary URL parameters, so the
 * player keeps a single source of truth: the URL. A switch therefore survives a
 * refresh, and the session gate keeps stopping the replaced session.
 */
export function usePlaybackVariantSwitch({
  itemId,
  playbackKey,
  param,
  requestedValue,
  canonicalValue,
  queryVariant,
  searchParams,
  setSearchParams,
  streamUrl,
  playing,
  playbackRate,
  sessionFailed,
  videoRef,
  reportPlayback,
  resetControlsTimeout,
  messages,
}: PlaybackVariantSwitchOptions): PlaybackVariantSwitch {
  const queryClient = useQueryClient();
  const canonicalize = canonicalValue ?? normalizeParam;
  const [switching, setSwitching] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const pendingRef = useRef<PendingResume | null>(null);
  const switchSequenceRef = useRef(0);
  const cancelSwitch = useCallback(() => {
    switchSequenceRef.current += 1;
    pendingRef.current = null;
    setSwitching(false);
    setNotice(null);
  }, []);
  const revertSwitch = useCallback((pending: PendingResume, message: string) => {
    switchSequenceRef.current += 1;
    pendingRef.current = null;
    setSwitching(false);
    setNotice(message);
    const reverted = new URLSearchParams(searchParams);
    if (pending.previousParam) reverted.set(param, pending.previousParam);
    else reverted.delete(param);
    setSearchParams(reverted);
    // Stop an in-flight replacement fetch without letting the observer refetch
    // it, then refresh only the previous variant for this item. Removing the
    // failed target here would immediately create its session again; the next
    // switch already drops stale variants before starting.
    void queryClient.cancelQueries({
      predicate: (query) => isPlaybackQueryFor(
        query.queryKey,
        pending.itemId,
        pending.targetParam ?? "default",
        queryVariant,
      ),
    });
    void queryClient.resetQueries({
      predicate: (query) => isPlaybackQueryFor(
        query.queryKey,
        pending.itemId,
        pending.previousParam ?? "default",
        queryVariant,
      ),
    });
  }, [param, queryClient, queryVariant, searchParams, setSearchParams]);
  const beginSwitch = useCallback((nextParams: URLSearchParams) => {
    if (switching) return;
    const currentParam = canonicalize(searchParams.get(param));
    // Cached bootstrap/session entries for other variants reference sessions
    // that were stopped when this player left them. Dropping them forces a
    // fresh session when the user later switches back.
    const currentVariant = currentParam ?? "default";
    queryClient.removeQueries({
      predicate: (query) => {
        const [prefix, queryItemId] = query.queryKey;
        if (queryItemId !== itemId) return false;
        if (!PLAYBACK_QUERY_PREFIXES.has(String(prefix))) return false;
        return queryVariant(query.queryKey) !== currentVariant;
      },
    });
    const video = videoRef.current;
    const position = video && Number.isFinite(video.currentTime) ? Math.max(0, video.currentTime) : 0;
    const volume = video && Number.isFinite(video.volume) ? Math.max(0, Math.min(1, video.volume)) : 1;
    const muted = video ? video.muted : false;
    const sequence = switchSequenceRef.current + 1;
    switchSequenceRef.current = sequence;
    pendingRef.current = {
      sequence,
      itemId,
      targetParam: canonicalize(nextParams.get(param)),
      position,
      playbackRate,
      volume,
      muted,
      playing,
      previousParam: currentParam,
      originStreamUrl: streamUrl,
      appliedPlaybackKey: null,
    };
    setNotice(null);
    setSwitching(true);
    resetControlsTimeout();
    // Persist the live position before the bootstrap request reads it back, so
    // a refresh and the fallback resume path agree on where playback was.
    const report = reportPlayback(playing ? "PLAYING" : "PAUSED", true);
    const apply = () => {
      // A timed-out/failed switch can leave this report in flight; never let it
      // resurrect the abandoned target URL.
      if (switchSequenceRef.current !== sequence || pendingRef.current?.sequence !== sequence) return;
      setSearchParams(nextParams);
    };
    if (report && typeof (report as Promise<unknown>).finally === "function") {
      void (report as Promise<unknown>).catch(() => undefined).finally(apply);
    } else {
      apply();
    }
  }, [
    canonicalize,
    itemId,
    param,
    playbackRate,
    playing,
    queryClient,
    queryVariant,
    reportPlayback,
    resetControlsTimeout,
    searchParams,
    setSearchParams,
    streamUrl,
    switching,
    videoRef,
  ]);

  useEffect(() => {
    const pending = pendingRef.current;
    if (!pending) return;
    if (pending.itemId !== itemId) {
      cancelSwitch();
      return;
    }
    if (canonicalize(requestedValue) !== pending.targetParam) {
      // The URL moved away from this switch (source/episode change), so the
      // captured position and rollback parameters belong to another media.
      cancelSwitch();
      return;
    }
    if (pending.appliedPlaybackKey === null) {
      pending.appliedPlaybackKey = playbackKey;
    } else if (pending.appliedPlaybackKey !== playbackKey) {
      cancelSwitch();
    }
  }, [cancelSwitch, canonicalize, itemId, playbackKey, requestedValue]);

  useEffect(() => {
    const pending = pendingRef.current;
    if (!pending || sessionFailed) return undefined;
    if (pending.itemId !== itemId) return undefined;
    if (pending.appliedPlaybackKey !== playbackKey) return undefined;
    if (canonicalize(requestedValue) !== pending.targetParam) return undefined;
    // Wait until the replacement session actually produced a different stream;
    // a legacy placeholder can still expose the previous URL for one render.
    if (!streamUrl || streamUrl === pending.originStreamUrl) return undefined;
    const video = videoRef.current;
    if (!video) return undefined;

    let cancelled = false;
    let applied = false;
    const apply = () => {
      if (cancelled || applied) return;
      if (pendingRef.current !== pending || switchSequenceRef.current !== pending.sequence) return;
      applied = true;
      if (Number.isFinite(pending.position) && pending.position > 0) {
        const maximum = Number.isFinite(video.duration) && video.duration > 0
          ? Math.max(0, video.duration - 0.25)
          : pending.position;
        const target = Math.min(pending.position, maximum);
        if (Math.abs(video.currentTime - target) > 0.25) video.currentTime = target;
      }
      video.playbackRate = pending.playbackRate;
      video.volume = pending.volume;
      video.muted = pending.muted;
      if (pending.playing) {
        void video.play().catch(() => {
          if (!cancelled) setNotice(messages.autoplayBlocked);
        });
      } else {
        video.pause();
      }
      pendingRef.current = null;
      setSwitching(false);
    };
    const applyWhenReady = () => {
      // `loadedmetadata` already ran the runtime's server-position restore;
      // wait for media data so this correction is applied afterwards.
      if (video.readyState >= 3) apply();
    };

    video.addEventListener("canplay", apply);
    video.addEventListener("loadeddata", apply);
    applyWhenReady();

    return () => {
      cancelled = true;
      video.removeEventListener("canplay", apply);
      video.removeEventListener("loadeddata", apply);
    };
  }, [
    canonicalize,
    itemId,
    messages.autoplayBlocked,
    playbackKey,
    requestedValue,
    sessionFailed,
    streamUrl,
    videoRef,
  ]);

  useEffect(() => {
    if (!switching) return undefined;
    // The replacement plan can be UNSUPPORTED and never produce a stream URL,
    // so the timeout must not depend on `streamUrl` changing.
    const timeout = window.setTimeout(() => {
      const pending = pendingRef.current;
      if (!pending) return;
      revertSwitch(pending, messages.timeout);
    }, RESUME_TIMEOUT_MS);
    return () => window.clearTimeout(timeout);
  }, [messages.timeout, revertSwitch, switching]);

  useEffect(() => {
    const pending = pendingRef.current;
    if (!pending || !sessionFailed) return;
    revertSwitch(pending, messages.failure);
  }, [messages.failure, revertSwitch, sessionFailed]);

  return { switching, notice, beginSwitch };
}
