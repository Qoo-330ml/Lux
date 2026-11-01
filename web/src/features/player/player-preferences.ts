import { api } from "../../lib/api/client";
import type { AccountPreferences, AccountPreferencesUpdate } from "../../lib/api/types";
import {
  DEFAULT_ACCOUNT_SETTINGS,
  readAccountSettings,
  saveAccountSettings,
  type AccountSettings,
} from "../account/account-settings";

/**
 * "自动连播下一集" reuses the existing account playback preference instead of a
 * new player-only flag, so the switch agrees with 账户设置 on every device. There
 * is no new backend field: the player only reads and patches the value that
 * already exists in the account preference payload.
 */
export const AUTO_PLAY_NEXT_EPISODE_DEFAULT = DEFAULT_ACCOUNT_SETTINGS.autoPlayNextEpisode;

export function readAutoPlayNextEpisodePreference(userId?: string): boolean {
  return readAccountSettings(userId).autoPlayNextEpisode !== false;
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
 * Best-effort cross-device save. The cached/local value is already applied when
 * this runs, so an offline or conflicting account API keeps the local choice and
 * only skips the server round-trip.
 */
export async function syncAutoPlayNextEpisodePreference(
  enabled: boolean,
  userId?: string,
): Promise<AccountPreferences | null> {
  if (!userId) return null;
  try {
    const preferences = await api.accountPreferences();
    if (preferences.autoPlayNextEpisode === enabled) return preferences;
    return await api.updateAccountPreferences(
      accountPreferencesUpdate(preferences, enabled),
    );
  } catch {
    return null;
  }
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
