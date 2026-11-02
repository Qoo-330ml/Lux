/**
 * React state for the browser-local danmaku settings.
 *
 * The stored copy is the source of truth on mount; every update writes through
 * (`saveDanmakuSettings`) so a reload keeps the viewer's choice. A disabled or full storage only
 * means the value is not persisted — the in-memory state still applies immediately.
 */

import { useCallback, useState } from "react";
import {
  DEFAULT_DANMAKU_SETTINGS,
  readDanmakuSettings,
  saveDanmakuSettings,
  type DanmakuSettings,
} from "./settings";

export type UseDanmakuSettingsOptions = {
  /** Injected in tests; defaults to this browser's `localStorage`. */
  storage?: Storage | null;
};

export type DanmakuSettingsStore = {
  settings: DanmakuSettings;
  /** Applies a patch, normalizes it, persists it, and returns the stored value. */
  updateSettings(patch: Partial<DanmakuSettings>): void;
  /** Writes the defaults back to storage and to the component state. */
  resetSettings(): void;
};

export function useDanmakuSettings(
  options: UseDanmakuSettingsOptions = {},
): DanmakuSettingsStore {
  const { storage } = options;
  const [settings, setSettings] = useState<DanmakuSettings>(() => readDanmakuSettings(storage));

  const updateSettings = useCallback((patch: Partial<DanmakuSettings>) => {
    setSettings((previous) => saveDanmakuSettings({ ...previous, ...patch }, storage));
  }, [storage]);

  const resetSettings = useCallback(() => {
    setSettings(saveDanmakuSettings(DEFAULT_DANMAKU_SETTINGS, storage));
  }, [storage]);

  return { settings, updateSettings, resetSettings };
}
