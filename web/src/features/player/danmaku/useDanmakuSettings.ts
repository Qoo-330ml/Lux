/**
 * React state for the browser-local danmaku settings.
 *
 * The stored copy is the source of truth on mount; every update writes through
 * (`saveDanmakuSettings`) so a reload keeps the viewer's choice. A disabled or full storage only
 * means the value is not persisted — the in-memory state still applies immediately.
 *
 * Storage writes happen *before* `setSettings` and never inside a state updater: React may invoke
 * an updater twice (StrictMode), discard a concurrent render, or replay it, and a side effect there
 * would write twice or persist a value that never reaches the screen. A ref mirrors the latest
 * committed value so a rapid sequence of updates still composes (each one reads the previous value).
 */

import { useCallback, useRef, useState } from "react";
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
  // Latest committed value. Only this hook changes `settings`, so writing the ref at commit time
  // (not during render) stays in sync and keeps consecutive updates from dropping each other.
  const settingsRef = useRef(settings);

  const commit = useCallback((next: DanmakuSettings) => {
    settingsRef.current = next;
    setSettings(next);
  }, []);

  const updateSettings = useCallback((patch: Partial<DanmakuSettings>) => {
    commit(saveDanmakuSettings({ ...settingsRef.current, ...patch }, storage));
  }, [commit, storage]);

  const resetSettings = useCallback(() => {
    commit(saveDanmakuSettings(DEFAULT_DANMAKU_SETTINGS, storage));
  }, [commit, storage]);

  return { settings, updateSettings, resetSettings };
}
