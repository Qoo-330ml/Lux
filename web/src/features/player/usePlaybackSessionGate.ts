import { useEffect, useRef } from "react";
import type { WebPlaybackSession } from "../../lib/api/types";

type Ref<T> = { current: T };

type PlaybackSessionGateOptions = {
  playbackKey: string;
  sessionGateKey: string;
  setSessionGateKey: (key: string) => void;
  playbackSession: WebPlaybackSession | undefined;
  playbackSessionIdRef: Ref<string | null>;
  sessionStartedRef: Ref<boolean>;
  stopActiveSession: (sessionId: string | null, keepalive?: boolean) => Promise<void>;
  resetSequence: () => void;
};

/**
 * Serializes session replacement. The previous session is stopped through a
 * promise chain before the gate key flips, so the next session query can never
 * start while the old session is still shutting down. The gate key only opens
 * for the transition that created it.
 */
export function usePlaybackSessionGate({
  playbackKey,
  sessionGateKey,
  setSessionGateKey,
  playbackSession,
  playbackSessionIdRef,
  sessionStartedRef,
  stopActiveSession,
  resetSequence,
}: PlaybackSessionGateOptions) {
  const sessionTransitionRef = useRef(Promise.resolve());

  useEffect(() => {
    if (sessionGateKey === playbackKey) return undefined;
    const previousSessionId = playbackSessionIdRef.current;
    playbackSessionIdRef.current = null;
    resetSequence();
    let active = true;
    sessionTransitionRef.current = sessionTransitionRef.current
      .catch(() => undefined)
      .then(() => previousSessionId ? stopActiveSession(previousSessionId) : undefined)
      .finally(() => {
        if (active) setSessionGateKey(playbackKey);
      });
    return () => {
      active = false;
    };
  }, [playbackKey, playbackSessionIdRef, resetSequence, sessionGateKey, setSessionGateKey, stopActiveSession]);

  useEffect(() => {
    if (sessionGateKey !== playbackKey) return;
    if (playbackSession?.sessionId) sessionStartedRef.current = true;
    playbackSessionIdRef.current = playbackSession?.sessionId ?? null;
    resetSequence();
  }, [playbackKey, playbackSession?.sessionId, playbackSessionIdRef, resetSequence, sessionGateKey, sessionStartedRef]);
}
