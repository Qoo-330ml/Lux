import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AccountPreferences, AuthSession } from "../../lib/api/types";
import {
  autoPlayNextEpisodeFromPreferences,
  hasStoredPlaybackPreferences,
  readAutoPlayNextEpisodePreference,
  syncAutoPlayNextEpisodePreference,
  writeAutoPlayNextEpisodePreference,
} from "./player-preferences";

/**
 * The player settings switch for "自动连播下一集". A stored local copy wins;
 * otherwise the account preference (cached query or one server read) seeds both
 * the switch and the local copy. Toggling applies immediately before a queued,
 * best-effort account sync.
 */
export function useAutoPlayNextPreference(): {
  autoPlayEnabled: boolean;
  setAutoPlayEnabled: (enabled: boolean) => void;
} {
  const queryClient = useQueryClient();
  // Reuse the session already cached by the app shell; the player never starts
  // its own /auth/me request.
  const cachedSession = useQuery({
    queryKey: queryKeys.me,
    queryFn: () => api.me(),
    enabled: false,
  });
  const userId = (cachedSession.data as AuthSession | undefined)?.user?.id;
  const [autoPlayEnabled, setAutoPlayEnabledState] = useState(
    () => readAutoPlayNextEpisodePreference(userId),
  );
  // Every user toggle is newer than a first-read response that is still in
  // flight. The counter is compared instead of the value because a toggle can
  // set the same value the server reports, and that equality must not revive the
  // stale response.
  const toggleSequenceRef = useRef(0);

  const needsAccountPreference = Boolean(userId) && !hasStoredPlaybackPreferences(userId);
  useEffect(() => {
    if (!userId || !needsAccountPreference) return undefined;
    const cached = queryClient.getQueryData<AccountPreferences>(
      queryKeys.accountPreferences(userId),
    );
    if (cached) {
      const enabled = autoPlayNextEpisodeFromPreferences(cached);
      setAutoPlayEnabledState(enabled);
      writeAutoPlayNextEpisodePreference(enabled, userId);
      return undefined;
    }
    let cancelled = false;
    const requestSequence = toggleSequenceRef.current;
    void api.accountPreferences()
      .then((preferences) => {
        // A toggle that landed after this read started wins over the response:
        // the response is older than the viewer's choice, so it seeds neither the
        // switch, nor the local copy, nor the account-preferences cache. Writing
        // the older version into that cache would make the next save submit a
        // stale expectedVersion and fight a 409.
        if (toggleSequenceRef.current !== requestSequence) return;
        queryClient.setQueryData<AccountPreferences>(
          queryKeys.accountPreferences(userId),
          preferences,
        );
        if (cancelled) return;
        const enabled = autoPlayNextEpisodeFromPreferences(preferences);
        setAutoPlayEnabledState(enabled);
        writeAutoPlayNextEpisodePreference(enabled, userId);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [needsAccountPreference, queryClient, userId]);

  const setAutoPlayEnabled = useCallback((enabled: boolean) => {
    // Any pending first read is now stale, whatever value it eventually reports.
    toggleSequenceRef.current += 1;
    setAutoPlayEnabledState(enabled);
    writeAutoPlayNextEpisodePreference(enabled, userId);
    if (!userId) return;
    void syncAutoPlayNextEpisodePreference(enabled, userId).then((saved) => {
      // `null` means the account API was offline or still in conflict: the local
      // value stays as the viewer left it and the cached server snapshot is left
      // untouched, so a later authoritative read still wins (see the tradeoff note
      // on syncAutoPlayNextEpisodePreference).
      if (!saved) return;
      queryClient.setQueryData<AccountPreferences>(
        queryKeys.accountPreferences(userId),
        saved,
      );
    });
  }, [queryClient, userId]);

  return { autoPlayEnabled, setAutoPlayEnabled };
}
