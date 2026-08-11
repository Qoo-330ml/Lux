export type LuxTheme = "light" | "dark";
export type LuxAccentColor = "berry" | "ocean" | "amber" | "mint";
export type LibraryMoveDirection = "up" | "down";

export type AccountSettings = {
  theme: LuxTheme;
  accentColor: LuxAccentColor;
  libraryOrder: string[];
  showMediaLibraries: boolean;
  showContinueWatching: boolean;
  audioLanguage: string;
  subtitleLanguage: string;
  autoPlayNextEpisode: boolean;
};

export const ACCOUNT_SETTINGS_STORAGE_KEY = "lux.account.settings";
export const ACCOUNT_AVATAR_STORAGE_KEY = "lux.account.avatar";

export const DEFAULT_ACCOUNT_SETTINGS: AccountSettings = {
  theme: "dark",
  accentColor: "berry",
  libraryOrder: [],
  showMediaLibraries: true,
  showContinueWatching: true,
  audioLanguage: "原始音轨",
  subtitleLanguage: "简体中文",
  autoPlayNextEpisode: true,
};

export function accountSettingsStorageKey(userId?: string): string {
  return userId ? `${ACCOUNT_SETTINGS_STORAGE_KEY}:${encodeURIComponent(userId)}` : ACCOUNT_SETTINGS_STORAGE_KEY;
}

export function accountAvatarStorageKey(userId?: string): string {
  return userId ? `${ACCOUNT_AVATAR_STORAGE_KEY}:${encodeURIComponent(userId)}` : ACCOUNT_AVATAR_STORAGE_KEY;
}

export function readAccountAvatar(userId?: string): string | null {
  const storage = getStorage();
  if (!storage) return null;

  try {
    const avatar = storage.getItem(accountAvatarStorageKey(userId));
    return avatar && isAvatarDataUrl(avatar) ? avatar : null;
  } catch {
    return null;
  }
}

export function saveAccountAvatar(dataUrl: string, userId?: string): boolean {
  const storage = getStorage();
  if (!storage || !isAvatarDataUrl(dataUrl)) return false;

  try {
    storage.setItem(accountAvatarStorageKey(userId), dataUrl);
    return true;
  } catch {
    return false;
  }
}

export function readAccountSettings(userId?: string): AccountSettings {
  const storage = getStorage();
  if (!storage) return { ...DEFAULT_ACCOUNT_SETTINGS };

  try {
    const stored = JSON.parse(storage.getItem(accountSettingsStorageKey(userId)) ?? "null") as Partial<AccountSettings> | null;
    if (!stored || typeof stored !== "object") return { ...DEFAULT_ACCOUNT_SETTINGS };

    return {
      theme: stored.theme === "light" ? "light" : "dark",
      accentColor: isAccentColor(stored.accentColor) ? stored.accentColor : DEFAULT_ACCOUNT_SETTINGS.accentColor,
      libraryOrder: Array.isArray(stored.libraryOrder)
        ? stored.libraryOrder.filter((id): id is string => typeof id === "string")
        : [],
      showMediaLibraries: stored.showMediaLibraries !== false,
      showContinueWatching: stored.showContinueWatching !== false,
      audioLanguage: typeof stored.audioLanguage === "string" ? stored.audioLanguage : DEFAULT_ACCOUNT_SETTINGS.audioLanguage,
      subtitleLanguage: typeof stored.subtitleLanguage === "string" ? stored.subtitleLanguage : DEFAULT_ACCOUNT_SETTINGS.subtitleLanguage,
      autoPlayNextEpisode: stored.autoPlayNextEpisode !== false,
    };
  } catch {
    return { ...DEFAULT_ACCOUNT_SETTINGS };
  }
}

export function saveAccountSettings(settings: AccountSettings, userId?: string): void {
  const storage = getStorage();
  if (!storage) return;

  try {
    storage.setItem(accountSettingsStorageKey(userId), JSON.stringify(settings));
  } catch {
    // Preferences are best-effort when browser storage is unavailable or full.
  }
}

export function moveLibrary(
  libraryIds: string[],
  index: number,
  direction: LibraryMoveDirection,
): string[] {
  const nextIndex = direction === "up" ? index - 1 : index + 1;
  if (index < 0 || index >= libraryIds.length || nextIndex < 0 || nextIndex >= libraryIds.length) {
    return libraryIds;
  }

  const next = [...libraryIds];
  [next[index], next[nextIndex]] = [next[nextIndex], next[index]];
  return next;
}

export function orderLibraries<T extends { id: string }>(libraries: T[], savedOrder: string[]): T[] {
  const positions = new Map(savedOrder.map((id, index) => [id, index]));
  return [...libraries].sort((left, right) => (positions.get(left.id) ?? Number.MAX_SAFE_INTEGER) - (positions.get(right.id) ?? Number.MAX_SAFE_INTEGER));
}

export function applyAccountTheme(theme: LuxTheme): void {
  if (typeof document !== "undefined") {
    document.documentElement.dataset.luxTheme = theme;
  }
}

export function applyAccountAccent(accentColor: LuxAccentColor): void {
  if (typeof document !== "undefined") {
    document.documentElement.dataset.luxAccent = accentColor;
  }
}

function isAccentColor(value: unknown): value is LuxAccentColor {
  return value === "berry" || value === "ocean" || value === "amber" || value === "mint";
}

function isAvatarDataUrl(value: string): boolean {
  return /^data:image\/(?:jpeg|png|webp);base64,[A-Za-z0-9+/]+=*$/.test(value);
}

function getStorage(): Storage | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}
