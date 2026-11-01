import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useState } from "react";
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
    void api.accountPreferences()
      .then((preferences) => {
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
    setAutoPlayEnabledState(enabled);
    writeAutoPlayNextEpisodePreference(enabled, userId);
    if (!userId) return;
    void syncAutoPlayNextEpisodePreference(enabled, userId).then((saved) => {
      if (!saved) return;
      queryClient.setQueryData<AccountPreferences>(
        queryKeys.accountPreferences(userId),
        saved,
      );
    });
  }, [queryClient, userId]);

  return { autoPlayEnabled, setAutoPlayEnabled };
}
