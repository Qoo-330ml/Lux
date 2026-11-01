import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useState } from "react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AccountPreferences, AuthSession } from "../../lib/api/types";
import {
  readAutoPlayNextEpisodePreference,
  syncAutoPlayNextEpisodePreference,
  writeAutoPlayNextEpisodePreference,
} from "./player-preferences";

/**
 * The player settings switch for "自动连播下一集". The value comes from the
 * account preference cache (or local storage), and toggling applies immediately
 * before a best-effort account sync.
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
