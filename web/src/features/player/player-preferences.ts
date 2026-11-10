import { ApiError, api } from "../../lib/api/client";
import type { AccountPreferences, AccountPreferencesUpdate } from "../../lib/api/types";
import {
  DEFAULT_ACCOUNT_SETTINGS,
  readAccountSettings,
  readStoredAccountSettings,
  saveAccountSettings,
  type AccountSettings,
} from "../account/account-settings";

/**
 * "自动连播下一集" reuses the existing account playback preference instead of a
 * new player-only flag. The player reads the local copy first and only falls back
 * to the account preference API when this browser has no stored copy yet; toggles
 * write both. There is no new backend field: only the value that already exists in
 * the account preference payload is read and patched.
 */
export const AUTO_PLAY_NEXT_EPISODE_DEFAULT = DEFAULT_ACCOUNT_SETTINGS.autoPlayNextEpisode;

export function readAutoPlayNextEpisodePreference(userId?: string): boolean {
  return readAccountSettings(userId).autoPlayNextEpisode !== false;
}

export function autoPlayNextEpisodeFromPreferences(preferences: AccountPreferences): boolean {
  return preferences.autoPlayNextEpisode !== false;
}

/** True when this browser already stored a copy for the account. */
export function hasStoredPlaybackPreferences(userId?: string): boolean {
  return readStoredAccountSettings(userId) !== null;
}

export function writeAutoPlayNextEpisodePreference(
  enabled: boolean,
  userId?: string,
): AccountSettings {
  const next: AccountSettings = {
    ...readAccountSettings(userId),
    autoPlayNextEpisode: enabled,
  };
  saveAccountSettings(next, userId);
  return next;
}

/**
 * Best-effort cross-device save. The local value is already applied when this
 * runs, so an offline account API keeps the local choice and only skips the
 * server round-trip. Concurrent toggles are queued so the newest value wins and
 * a version conflict re-reads once before giving up.
 *
 * Failure policy: offline, and a 409 that survives that single retry, both end as
 * `null` — nothing is thrown, nothing is rolled back, and no new player UI is
 * added. That is a deliberate divergence from the account page
 * (`AccountPage.tsx`, LUX-658), which blocks further saves on a version conflict
 * and asks the viewer to refresh before retrying: the account page owns the whole
 * preference object and is where a human can actually resolve the conflict, while
 * the player only owns this one switch and must not grow a competing conflict
 * prompt. Keeping the local value also means this browser may show a value the
 * server does not have yet; that divergence is one-sided on purpose:
 * - a failed sync never overwrites a newer server version. The next authoritative
 *   read — the account page refetching and saving (which writes the server
 *   snapshot back through `saveAccountSettings`), or this browser losing its local
 *   copy — seeds from the server, so "server wins when it has a newer version"
 *   stays the rule;
 * - retries never resurrect an abandoned local value on their own: the queue only
 *   sends values the viewer picked, and a conflict re-read is discarded instead of
 *   being written into the local copy.
 */
let preferenceSyncQueue: Promise<unknown> = Promise.resolve();

export function syncAutoPlayNextEpisodePreference(
  enabled: boolean,
  userId?: string,
): Promise<AccountPreferences | null> {
  if (!userId) return Promise.resolve(null);
  const task = preferenceSyncQueue
    .catch(() => undefined)
    .then(() => syncAutoPlayNextEpisodePreferenceOnce(enabled));
  preferenceSyncQueue = task.then(() => undefined, () => undefined);
  return task;
}

async function syncAutoPlayNextEpisodePreferenceOnce(
  enabled: boolean,
): Promise<AccountPreferences | null> {
  // Never rejects: the caller already applied the local value, so every failure is
  // reported as `null` ("server copy left alone") per the tradeoff note above.
  for (let attempt = 0; attempt < 2; attempt += 1) {
    try {
      const preferences = await api.accountPreferences();
      if (preferences.autoPlayNextEpisode === enabled) return preferences;
      return await api.updateAccountPreferences(
        accountPreferencesUpdate(preferences, enabled),
      );
    } catch (error) {
      if (attempt === 0 && isVersionConflict(error)) continue;
      return null;
    }
  }
  return null;
}

function isVersionConflict(error: unknown): boolean {
  return error instanceof ApiError && error.code === "VERSION_CONFLICT";
}

function accountPreferencesUpdate(
  preferences: AccountPreferences,
  enabled: boolean,
): AccountPreferencesUpdate {
  return {
    theme: preferences.theme,
    accentColor: preferences.accentColor,
    showMediaLibraries: preferences.showMediaLibraries,
    showContinueWatching: preferences.showContinueWatching,
    audioLanguage: preferences.audioLanguage,
    subtitleLanguage: preferences.subtitleLanguage,
    autoPlayNextEpisode: enabled,
    libraryOrder: preferences.libraryOrder,
    playedPercent: preferences.playedPercent,
    expectedVersion: preferences.version,
  };
}
