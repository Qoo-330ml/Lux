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
